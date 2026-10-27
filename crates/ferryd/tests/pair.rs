//! SAS pairing through the socket API: two in-process daemons on
//! loopback, a man in the middle, and every way an attempt can end without a pin.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use bytes::Bytes;
use ferry_core::PeerId;
use ferry_core::pair::{
    PairAction, PairEvent, PairFrame, PairMachine, PairStage, Role, Start, commitment,
};
use ferry_core::wire::control::Pair;
use ferry_core::{Actions, Micros};
use ferry_net::{Endpoint, EndpointConfig, Event, Identity, PairingConnection, TrustSet};
use serde_json::{Value, json};
use support::*;

/// A socket client for a pairing: the responses to its requests, and the `pairing` events that
/// come between them.
struct PairClient {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    events: VecDeque<Value>,
}

impl PairClient {
    fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        Self {
            reader,
            writer: stream,
            next_id: 1,
            events: VecDeque::new(),
        }
    }

    fn line(&mut self, timeout: Duration) -> Option<Value> {
        self.reader
            .get_ref()
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))
            .unwrap();
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(serde_json::from_str(&line).unwrap()),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                None
            }
            Err(e) => panic!("{e}"),
        }
    }

    /// The whole response line to one request; events that come first are kept.
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut req =
            serde_json::to_vec(&json!({"id": id, "method": method, "params": params})).unwrap();
        req.push(b'\n');
        self.writer.write_all(&req).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no answer to {method}");
            let Some(v) = self.line(left) else { continue };
            if v.get("event").is_some() {
                self.events.push_back(v);
            } else {
                assert_eq!(v["id"], id, "{v}");
                return v;
            }
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params);
        assert!(v.get("error").is_none(), "{method}: {v}");
        v["result"].clone()
    }

    fn error(&mut self, method: &str, params: Value) -> String {
        let v = self.call(method, params);
        v["error"]["code"]
            .as_str()
            .unwrap_or_else(|| panic!("{method} should fail: {v}"))
            .to_string()
    }

    /// The next `pairing` event's data, within `timeout`.
    fn event(&mut self, timeout: Duration) -> Option<Value> {
        if let Some(v) = self.events.pop_front() {
            return Some(v["data"].clone());
        }
        let v = self.line(timeout)?;
        assert_eq!(v["event"], "pairing", "{v}");
        Some(v["data"].clone())
    }

    /// Events until one at `stage`; returns it. Panics on a `failed` or `closed` when that is not
    /// what it waits for.
    fn until_stage(&mut self, stage: &str) -> Value {
        loop {
            let ev = self
                .event(Duration::from_secs(10))
                .unwrap_or_else(|| panic!("no {stage} event"));
            if ev["stage"] == stage {
                return ev;
            }
            assert!(
                !matches!(ev["stage"].as_str(), Some("failed" | "closed")),
                "waiting for {stage}: {ev}"
            );
        }
    }
}

fn trusted(env: &Env) -> Option<String> {
    std::fs::read_to_string(env.paths().trusted_file()).ok()
}

fn peer<'a>(status: &'a Value, fp: &str) -> Option<&'a Value> {
    status["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["fingerprint"] == fp)
}

/// B opens a window, A dials it; both reach the code. Returns (A's code event, B's).
fn to_the_code(
    a: &mut PairClient,
    b: &mut PairClient,
    b_addr: std::net::SocketAddr,
) -> (Value, Value) {
    let r = b.ok("pair.accept", json!(null));
    assert_eq!(r["expires_ms"], 120_000);
    a.ok("pair.start", json!({"addr": b_addr.to_string()}));
    (a.until_stage("code"), b.until_stage("code"))
}

#[test]
fn pair_then_connect() {
    let (ea, eb) = (Env::new(), Env::new());
    let mut oa = options(&ea);
    oa.name = Some("desktop".into());
    let mut ob = options(&eb);
    ob.name = Some("laptop".into());
    let (da, db) = (
        ferryd::Daemon::start(oa).unwrap(),
        ferryd::Daemon::start(ob).unwrap(),
    );
    let (fa, fb) = (da.fingerprint().to_string(), db.fingerprint().to_string());
    let (mut a, mut b) = (
        PairClient::connect(da.socket_path()),
        PairClient::connect(db.socket_path()),
    );

    let (ca, cb) = to_the_code(&mut a, &mut b, db.net_addr());
    // the same six digits on both, each naming the other with its key and address
    let code = ca["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 6);
    assert_eq!(ca["code"], cb["code"]);
    assert_eq!(ca["peer"]["name"], "laptop");
    assert_eq!(ca["peer"]["fingerprint"], fb);
    assert_eq!(cb["peer"]["name"], "desktop");
    assert_eq!(cb["peer"]["fingerprint"], fa);
    assert_eq!(cb["peer"]["addr"], da.net_addr().to_string());
    // status shows the stage, never the code
    let st = a.ok("status", json!(null));
    assert_eq!(st["pairing"]["stage"], "code");
    assert_eq!(st["pairing"]["role"], "dialer");
    assert!(!st.to_string().contains(&code));
    assert!(
        trusted(&ea).is_none() && trusted(&eb).is_none(),
        "nothing pinned yet"
    );

    a.ok("pair.answer", json!({"match": true}));
    assert_eq!(a.until_stage("waiting_peer")["peer"]["name"], "laptop");
    b.ok("pair.answer", json!({"match": true}));
    let pa = a.until_stage("paired");
    let pb = b.until_stage("paired");
    assert_eq!(pa["peer"]["fingerprint"], fb);
    assert_eq!(pb["peer"]["fingerprint"], fa);

    // both pinned, as SAS pins; B stored A's address from the pairing connection
    let ta = trusted(&ea).expect("A pinned B");
    let tb = trusted(&eb).expect("B pinned A");
    assert!(ta.contains(&fb) && ta.contains("paired = \"sas\""), "{ta}");
    assert!(
        tb.contains(&fa) && tb.contains(&da.net_addr().to_string()),
        "{tb}"
    );

    // then they connect on ferry/1 within 2 s of the pin
    let pinned = Instant::now();
    let mut ctl_a = Client::connect(da.socket_path());
    let mut ctl_b = Client::connect(db.socket_path());
    wait_for("connected on both", || {
        let sa = ctl_a.result("status", json!(null));
        let sb = ctl_b.result("status", json!(null));
        (peer(&sa, &fb)?["state"] == "connected" && peer(&sb, &fa)?["state"] == "connected")
            .then_some(())
    });
    assert!(
        pinned.elapsed() < Duration::from_secs(2),
        "{:?}",
        pinned.elapsed()
    );
    // `peers` lists the other on both, how it was paired and when it was last seen
    let pa = ctl_a.result("peers", json!(null));
    let entry = &pa["peers"][0];
    assert_eq!(entry["name"], "laptop");
    assert_eq!(entry["paired"], "sas");
    assert_eq!(entry["state"], "connected");
    assert!(entry["last_seen"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        ctl_b.result("peers", json!(null))["peers"][0]["name"],
        "desktop"
    );
    let st = ctl_a.result("status", json!(null));
    assert!(st.get("pairing").is_none(), "the pairing is over: {st}");
    assert_eq!(st["counters"]["pair_attempts"], 1);
    assert_eq!(st["counters"]["pair_failures"], 0);

    // pairing a pinned peer again is allowed: the pin stays one
    let (ca, _) = to_the_code(&mut a, &mut b, db.net_addr());
    assert!(ca["code"].is_string());
    a.ok("pair.answer", json!({"match": true}));
    b.ok("pair.answer", json!({"match": true}));
    a.until_stage("paired");
    b.until_stage("paired");
    assert_eq!(
        ctl_a.result("peers", json!(null))["peers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // unpair: the pin goes, and the connection closes with code 4, which is the revoke: B
    // removes its pin of A too, so `ferry peers` no longer lists it there
    let r = ctl_a.result("unpair", json!({"peer": "laptop"}));
    assert_eq!(r["peer"]["fingerprint"], fb);
    assert!(
        ctl_a.result("peers", json!(null))["peers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    wait_for("B drops its pin of A", || {
        ctl_b.result("peers", json!(null))["peers"]
            .as_array()
            .unwrap()
            .is_empty()
            .then_some(())
    });
    assert!(!trusted(&ea).unwrap().contains(&fb));
    assert!(!trusted(&eb).unwrap_or_default().contains(&fa));
    da.shutdown();
    db.shutdown();
}

/// The origin `layout` lists for `fp`, or `None` when it is not placed (or not listed).
fn origin_of(layout: &Value, fp: &str) -> Option<(i64, i64)> {
    let o = layout["peers"]
        .as_array()?
        .iter()
        .find(|p| p["fingerprint"] == fp)?
        .get("origin")?
        .as_array()?
        .clone();
    Some((o[0].as_i64()?, o[1].as_i64()?))
}

/// The next `layout` event whose data passes `f`, within 5 s.
fn layout_event(c: &mut Client, what: &str, f: impl Fn(&Value) -> bool) -> Value {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let ev = c
            .event_within("layout", left)
            .unwrap_or_else(|| panic!("no layout event: {what}"));
        if f(&ev["data"]) {
            return ev["data"].clone();
        }
    }
}

/// The peer that started the
/// pairing places the new one right of its group, tops aligned, at the first connection, and an
/// unpair removes the peer from the layout on both sides. Both are layout revisions, so the
/// subscribers get a `layout` event.
#[test]
fn a_new_peer_is_placed_right_of_the_initiator_and_an_unpaired_one_leaves_the_layout() {
    let (ea, eb) = (Env::new(), Env::new());
    let mut oa = options(&ea);
    oa.name = Some("desktop".into());
    let mut ob = options(&eb);
    ob.name = Some("laptop".into());
    ob.monitors = ferryd::MonitorSource::Fixed(vec![mon("eDP-1", 0, 0, 1366, 768)]);
    let (da, db) = (
        ferryd::Daemon::start(oa).unwrap(),
        ferryd::Daemon::start(ob).unwrap(),
    );
    let (fa, fb) = (da.fingerprint().to_string(), db.fingerprint().to_string());
    let (mut ctl_a, mut ctl_b) = (
        Client::connect(da.socket_path()),
        Client::connect(db.socket_path()),
    );
    // `ctl_*` only listen; requests go on `req_*`, so no event is read past
    ctl_a.subscribe(&["layout"]);
    ctl_b.subscribe(&["layout"]);
    let (mut req_a, mut req_b) = (
        Client::connect(da.socket_path()),
        Client::connect(db.socket_path()),
    );
    assert_eq!(
        req_a.result("layout.get", json!(null))["revision"]["counter"],
        0
    );

    let (mut a, mut b) = (
        PairClient::connect(da.socket_path()),
        PairClient::connect(db.socket_path()),
    );
    to_the_code(&mut a, &mut b, db.net_addr());
    a.ok("pair.answer", json!({"match": true}));
    b.ok("pair.answer", json!({"match": true}));
    a.until_stage("paired");
    b.until_stage("paired");

    // A started the pairing: B goes right of A's two monitors (3840 wide), tops aligned, in a
    // revision A made; B gets it over the new link
    let placed = |l: &Value| origin_of(l, &fb) == Some((3840, 0));
    for (c, who) in [(&mut ctl_a, "A"), (&mut ctl_b, "B")] {
        let l = layout_event(c, &format!("{who} sees B placed"), placed);
        assert_eq!(origin_of(&l, &fa), Some((0, 0)), "{who}: {l}");
        assert_eq!(l["revision"]["peer"], fa, "{who}: A made the edit: {l}");
    }
    let rev = req_a.result("layout.get", json!(null))["revision"]["counter"]
        .as_u64()
        .unwrap();
    assert_eq!(rev, 1, "one revision for the placement");

    // a later connection does not place it again: moved by hand, it stays where the user put it
    req_a.result(
        "layout.set",
        json!({"origins": [{"peer": "laptop", "x": -1366, "y": 100}]}),
    );
    layout_event(&mut ctl_b, "B sees the edit", |l| {
        origin_of(l, &fb) == Some((-1366, 100))
    });

    // B unpairs A: B's layout loses A at once; A, told by the revoke, removes B from its own
    req_b.result("unpair", json!({"peer": "desktop"}));
    let l = layout_event(&mut ctl_b, "B drops A", |l| origin_of(l, &fa).is_none());
    assert_eq!(l["revision"]["peer"], fb, "{l}");
    layout_event(&mut ctl_a, "A drops B", |l| origin_of(l, &fb).is_none());
    da.shutdown();
    db.shutdown();
}

/// Every string in `v`, recursively.
fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().flat_map(strings).collect(),
        Value::Object(o) => o.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

fn no_controls(ev: &Value) {
    for s in strings(ev) {
        assert!(!s.chars().any(char::is_control), "{s:?} in {ev}");
    }
}

/// A `Hello.name` with control characters never reaches an event; the peer is pinned
/// under its default name, and the note says the name was not allowed, not that it is taken.
/// A valid name that another peer has keeps that note.
#[test]
fn a_peer_with_an_invalid_name_is_pinned_under_a_default_and_told_why() {
    let (ea, eb) = (Env::new(), Env::new());
    let mut oa = options(&ea);
    oa.name = Some("A\u{1b}[1;31m\u{7}".into());
    let mut ob = options(&eb);
    ob.name = Some("laptop".into());
    let (da, db) = (
        ferryd::Daemon::start(oa).unwrap(),
        ferryd::Daemon::start(ob).unwrap(),
    );
    let default_a = ferry_core::PeerId(da.fingerprint().0).default_name();
    let (mut a, mut b) = (
        PairClient::connect(da.socket_path()),
        PairClient::connect(db.socket_path()),
    );
    let (ca, cb) = to_the_code(&mut a, &mut b, db.net_addr());
    assert_eq!(cb["peer"]["name"], default_a.as_str());
    assert_eq!(ca["peer"]["name"], "laptop");
    a.ok("pair.answer", json!({"match": true}));
    b.ok("pair.answer", json!({"match": true}));
    let pa = a.until_stage("paired");
    let pb = b.until_stage("paired");
    for ev in [&ca, &cb, &pa, &pb] {
        no_controls(ev);
    }
    assert_eq!(pb["peer"]["name"], default_a.as_str());
    assert_eq!(
        pb["message"],
        format!(
            "pinned as {default_a}: the name it sent is not allowed; \
             a peer name cannot contain '\\u{{1b}}' (no whitespace, control characters or ':')"
        )
    );
    assert!(pa.get("message").is_none(), "{pa}");
    da.shutdown();

    // A valid name another peer already has: the note says so.
    let ec = Env::new();
    let mut oc = options(&ec);
    oc.name = Some("taken".into());
    let dc = ferryd::Daemon::start(oc).unwrap();
    let mut c = PairClient::connect(dc.socket_path());
    b.ok(
        "trust",
        json!({"fingerprint": "11".repeat(32), "name": "taken"}),
    );
    let (_, cb) = to_the_code(&mut c, &mut b, db.net_addr());
    assert_eq!(cb["peer"]["name"], "taken");
    c.ok("pair.answer", json!({"match": true}));
    b.ok("pair.answer", json!({"match": true}));
    c.until_stage("paired");
    let pb = b.until_stage("paired");
    assert_eq!(
        pb["message"],
        "pinned as taken-2: the name taken belongs to another peer"
    );
    dc.shutdown();
    db.shutdown();
}

#[test]
fn a_dialer_without_a_window_there_is_told_not_accepting() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (start(&ea), start(&eb));
    let mut a = PairClient::connect(da.socket_path());
    let started = Instant::now();
    let v = a.call("pair.start", json!({"addr": db.net_addr().to_string()}));
    assert_eq!(v["error"]["code"], "pairing_not_accepting", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is not accepting pairing")
    );
    assert!(
        v["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("ferry pair --accept")
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    // nothing runs any more on A
    assert!(a.ok("status", json!(null)).get("pairing").is_none());
    let mut b = Client::connect(db.socket_path());
    wait_for("B counted the refusal", || {
        (b.result("status", json!(null))["counters"]["pairing_refused"] == 1).then_some(())
    });
    da.shutdown();
    db.shutdown();
}

#[test]
fn one_pairing_per_machine_and_answers_only_on_its_connection() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (start(&ea), start(&eb));
    let mut b = PairClient::connect(db.socket_path());
    b.ok("pair.accept", json!(null));
    let mut other = PairClient::connect(db.socket_path());
    assert_eq!(other.error("pair.accept", json!(null)), "pairing_busy");
    assert_eq!(
        other.error("pair.start", json!({"addr": da.net_addr().to_string()})),
        "pairing_busy"
    );
    assert_eq!(
        other.error("pair.answer", json!({"match": true})),
        "no_pairing"
    );
    // (`pair.cancel` is not refused to another connection any more: tests/pair_end.rs)
    // no code yet: nothing to answer
    assert_eq!(b.error("pair.answer", json!({"match": true})), "no_pairing");
    assert_eq!(
        other.error("pair.start", json!({"addr": "not a host"})),
        "bad_params"
    );
    let st = other.ok("status", json!(null));
    assert_eq!(st["pairing"]["role"], "acceptor");
    assert_eq!(st["pairing"]["stage"], "listening");
    // cancelling the window ends it
    b.ok("pair.cancel", json!(null));
    assert_eq!(b.until_stage("closed")["reason"], "cancelled");
    assert!(other.ok("status", json!(null)).get("pairing").is_none());
    da.shutdown();
    db.shutdown();
}

/// Each way an attempt ends without a pin, on each side: trusted.toml stays absent on both.
#[test]
fn pair_cancel_timeout_drop_pin_nothing() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (start(&ea), start(&eb));
    let b_addr = db.net_addr();
    let mut b = PairClient::connect(db.socket_path());
    let nothing = |ea: &Env, eb: &Env| {
        assert!(trusted(ea).is_none(), "A pinned something");
        assert!(trusted(eb).is_none(), "B pinned something");
    };

    // 1. A says the codes differ: both "cancelled: codes differ"; B keeps listening
    let mut a = PairClient::connect(da.socket_path());
    to_the_code(&mut a, &mut b, b_addr);
    a.ok("pair.answer", json!({"match": false}));
    let fa = a.until_stage("failed");
    assert_eq!(
        (fa["reason"].as_str(), fa["message"].as_str()),
        (Some("rejected"), Some("cancelled: codes differ"))
    );
    let fb = b.until_stage("failed");
    assert_eq!(fb["reason"], "rejected");
    b.until_stage("listening");
    nothing(&ea, &eb);

    // 2. A cancels (pair.cancel) while both compare
    a.ok("pair.start", json!({"addr": b_addr.to_string()}));
    a.until_stage("code");
    b.until_stage("code");
    a.ok("pair.cancel", json!(null));
    assert_eq!(a.until_stage("failed")["reason"], "cancelled");
    assert_eq!(b.until_stage("failed")["reason"], "cancelled");
    // that was B's second failure; B's window stays open for a third
    b.until_stage("listening");
    nothing(&ea, &eb);

    // 3. A's socket connection closes (Ctrl+C) while B waits for A
    drop(a);
    let mut a = PairClient::connect(da.socket_path());
    a.ok("pair.start", json!({"addr": b_addr.to_string()}));
    a.until_stage("code");
    b.until_stage("code");
    b.ok("pair.answer", json!({"match": true}));
    b.until_stage("waiting_peer");
    drop(a);
    assert_eq!(b.until_stage("failed")["reason"], "cancelled");
    // three failures close the window
    assert_eq!(b.until_stage("closed")["reason"], "failures");
    nothing(&ea, &eb);
    let mut a = PairClient::connect(da.socket_path());
    // A's pairing is over when its attempt has closed the connection and net has told ctl, which
    // is after `close_graceful` (up to 500 ms) returned. B learns of the cancel from the frame
    // that goes first, so B's `closed` can come before that: `status` is waited for, not read once.
    wait_for("A's pairing gone from status", || {
        a.ok("status", json!(null))
            .get("pairing")
            .is_none()
            .then_some(())
    });

    // Each case below gets its own B: an address may start 3 pairing attempts a minute.
    // 4. B cancels while A waits for B: A fails, B's window closes at once
    let eb = Env::new();
    let db = start(&eb);
    let mut b = PairClient::connect(db.socket_path());
    to_the_code(&mut a, &mut b, db.net_addr());
    a.ok("pair.answer", json!({"match": true}));
    a.until_stage("waiting_peer");
    b.ok("pair.cancel", json!(null));
    assert_eq!(a.until_stage("failed")["reason"], "cancelled");
    assert_eq!(b.until_stage("failed")["reason"], "cancelled");
    assert_eq!(b.until_stage("closed")["reason"], "cancelled");
    nothing(&ea, &eb);
    db.shutdown();

    // 5. B's socket closes while A compares: A fails
    let eb = Env::new();
    let db = start(&eb);
    let mut b = PairClient::connect(db.socket_path());
    to_the_code(&mut a, &mut b, db.net_addr());
    drop(b);
    assert_eq!(a.until_stage("failed")["reason"], "cancelled");
    nothing(&ea, &eb);
    db.shutdown();

    // 6. the link drops: B's daemon stops while A waits; A pins nothing
    let eb = Env::new();
    let db = start(&eb);
    let mut b = PairClient::connect(db.socket_path());
    to_the_code(&mut a, &mut b, db.net_addr());
    a.ok("pair.answer", json!({"match": true}));
    a.until_stage("waiting_peer");
    db.shutdown();
    let f = a.until_stage("failed");
    assert!(
        matches!(f["reason"].as_str(), Some("lost" | "cancelled")),
        "{f}"
    );
    nothing(&ea, &eb);
    da.shutdown();
}

/// A machine step that takes over 5 s ends the attempt as `timeout`. The dialer here is
/// a raw ferry-pair/1 connection that sends its Hello and then nothing.
#[test]
fn a_silent_dialer_times_out() {
    let eb = Env::new();
    let db = start(&eb);
    let mut b = PairClient::connect(db.socket_path());
    b.ok("pair.accept", json!(null));
    let rt = rt();
    let id = Identity::ephemeral().unwrap();
    let ep = {
        let _g = rt.enter();
        Endpoint::bind(id, TrustSet::new(), lo_cfg()).unwrap()
    };
    let pc = rt.block_on(ep.dial_pairing(db.net_addr())).unwrap();
    send(
        &pc,
        &PairFrame::Control(ferryd::fake_peer::hello_now("silent", vec![])),
    );
    let started = Instant::now();
    let f = b.until_stage("failed");
    assert_eq!(f["reason"], "timeout");
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(4500) && took < Duration::from_secs(7),
        "{took:?}"
    );
    b.until_stage("listening");
    assert!(trusted(&eb).is_none());
    drop(pc);
    db.shutdown();
}

fn lo_cfg() -> EndpointConfig {
    EndpointConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        ..EndpointConfig::default()
    }
}

fn send(pc: &PairingConnection, f: &PairFrame) {
    let mut b = Vec::new();
    f.encode(&mut b);
    pc.conn.try_send_frame(Bytes::from(b)).unwrap();
}

/// M's half of one session: a real PairMachine over a real pairing connection.
async fn run_side(
    m: &mut PairMachine,
    pc: &mut PairingConnection,
    out: &mut Actions<PairAction>,
    until_code: bool,
) -> Option<u32> {
    let mut code = None;
    loop {
        for a in out.drain().collect::<Vec<_>>() {
            match a {
                PairAction::Send(f) => send(pc, &f),
                PairAction::ShowCode { code: c, .. } => code = Some(c.0),
                PairAction::Failed(_) | PairAction::Close { .. } => return code,
                PairAction::Agreed { .. } => return code,
            }
        }
        if until_code && code.is_some() {
            return code;
        }
        let ev = tokio::time::timeout(Duration::from_secs(10), pc.events.next())
            .await
            .ok()??;
        match ev {
            Event::Frame(b) => m.handle(
                PairEvent::Frame(PairFrame::decode(&b).unwrap()),
                Micros(1),
                out,
            ),
            Event::Closed(_) => {
                m.handle(PairEvent::Closed, Micros(1), out);
            }
            Event::Datagram(_) => {}
        }
    }
}

/// M sits between A and B with its own key: A dials M (thinking it is B), M dials B. M runs an
/// honest machine on each side, so both reach a code; the codes differ, the users see it and
/// say no, and nothing is pinned anywhere.
#[test]
fn pair_mitm_codes_differ() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (start(&ea), start(&eb));
    let mut a = PairClient::connect(da.socket_path());
    let mut b = PairClient::connect(db.socket_path());
    b.ok("pair.accept", json!(null));

    let rt = rt();
    let m_id = Identity::ephemeral().unwrap();
    let m_fp = m_id.fingerprint();
    let m_ep = {
        let _g = rt.enter();
        Endpoint::bind(m_id, TrustSet::new(), lo_cfg()).unwrap()
    };
    m_ep.set_pairing(Some(ferry_net::PairingWindow {
        until: Instant::now() + Duration::from_secs(60),
    }));
    let m_addr = m_ep.local_addr().unwrap();
    let (a_fp, b_fp, b_addr) = (da.fingerprint(), db.fingerprint(), db.net_addr());

    // M's two sessions, each with its own machine and nonce
    let m_task = {
        let m_ep = m_ep.clone();
        rt.spawn(async move {
            let mut to_a = m_ep.accept_pairing().await.unwrap();
            assert_eq!(to_a.peer_fp(), a_fp);
            let mut to_b = m_ep.dial_pairing(b_addr).await.unwrap();
            assert_eq!(to_b.peer_fp(), b_fp);
            let hello = |n: &str| ferryd::fake_peer::hello_now(n, vec![]);
            let (mut ma, mut mb) = (PairMachine::new(), PairMachine::new());
            let (mut oa, mut ob) = (Actions::new(), Actions::new());
            ma.handle(
                PairEvent::Start(Start {
                    role: Role::Acceptor,
                    me: PeerId(m_fp.0),
                    peer: PeerId(a_fp.0),
                    nonce: [0x11; 32],
                    until: Micros(u64::MAX / 2),
                    hello: hello("laptop"),
                }),
                Micros(1),
                &mut oa,
            );
            mb.handle(
                PairEvent::Start(Start {
                    role: Role::Dialer,
                    me: PeerId(m_fp.0),
                    peer: PeerId(b_fp.0),
                    nonce: [0x22; 32],
                    until: Micros(u64::MAX / 2),
                    hello: hello("desktop"),
                }),
                Micros(1),
                &mut ob,
            );
            let code_b = run_side(&mut mb, &mut to_b, &mut ob, true).await;
            let code_a = run_side(&mut ma, &mut to_a, &mut oa, true).await;
            assert_eq!(ma.stage(), PairStage::Comparing);
            assert_eq!(mb.stage(), PairStage::Comparing);
            // keep both connections up until the users have answered
            let _ = tokio::join!(
                run_side(&mut ma, &mut to_a, &mut oa, false),
                run_side(&mut mb, &mut to_b, &mut ob, false)
            );
            (code_a, code_b)
        })
    };

    a.ok("pair.start", json!({"addr": m_addr.to_string()}));
    let ca = a.until_stage("code");
    let cb = b.until_stage("code");
    // A sees M's key where B's should be, and B sees M's key where A's should be
    assert_eq!(ca["peer"]["fingerprint"], m_fp.to_string());
    assert_eq!(cb["peer"]["fingerprint"], m_fp.to_string());
    assert_ne!(
        ca["code"], cb["code"],
        "M cannot steer the two codes together"
    );
    // the users compare, see two codes, and say no on both sides
    a.ok("pair.answer", json!({"match": false}));
    b.ok("pair.answer", json!({"match": false}));
    a.until_stage("failed");
    b.until_stage("failed");
    let (code_a, code_b) = rt.block_on(m_task).unwrap();
    assert_eq!(
        code_a.map(|c| format!("{c:06}")),
        ca["code"].as_str().map(str::to_string)
    );
    assert_eq!(
        code_b.map(|c| format!("{c:06}")),
        cb["code"].as_str().map(str::to_string)
    );
    assert!(trusted(&ea).is_none() && trusted(&eb).is_none());
    da.shutdown();
    db.shutdown();
}

/// A relay that forwards A's frames to B verbatim cannot reuse A's commitment: it binds A's
/// and M's keys, and B checks it against M's and B's. B ends the attempt as `bad_reveal`
/// before any code is shown.
#[test]
fn a_relayed_commitment_is_a_bad_reveal() {
    let eb = Env::new();
    let db = start(&eb);
    let mut b = PairClient::connect(db.socket_path());
    b.ok("pair.accept", json!(null));
    let rt = rt();
    let m_ep = {
        let _g = rt.enter();
        Endpoint::bind(Identity::ephemeral().unwrap(), TrustSet::new(), lo_cfg()).unwrap()
    };
    let a_fp = PeerId([0xa1; 32]);
    let m_fp = PeerId(m_ep.fingerprint().0);
    let n_a = [7u8; 32];
    let pc = rt.block_on(m_ep.dial_pairing(db.net_addr())).unwrap();
    // what A would have sent toward M: a commitment to (fp_A, fp_M, n_A)
    send(
        &pc,
        &PairFrame::Control(ferryd::fake_peer::hello_now("a", vec![])),
    );
    send(
        &pc,
        &PairFrame::Pair(Pair::Commit {
            commitment: commitment(&a_fp, &m_fp, &n_a),
        }),
    );
    let mut pc = pc;
    rt.block_on(async {
        // B's Hello, then B's Nonce
        for _ in 0..2 {
            let _ = tokio::time::timeout(Duration::from_secs(5), pc.events.next()).await;
        }
    });
    send(&pc, &PairFrame::Pair(Pair::Reveal { nonce: n_a }));
    let f = b.until_stage("failed");
    assert_eq!(f["reason"], "bad_reveal");
    assert!(trusted(&eb).is_none());
    drop(pc);
    db.shutdown();
}
