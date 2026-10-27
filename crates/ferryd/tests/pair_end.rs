//! How a pairing ends (pair-polish, m3 design section 9). Two promises, tested on real daemons:
//!
//! - **One last event.** However a pairing ends, everyone who hears its events hears exactly one
//!   last one, after which nothing more comes for it. A dialer's last event is `paired` or
//!   `failed`; an acceptor's is `paired` or `closed` (its `failed` ends one attempt, and
//!   `listening` or `closed` follows). The table below drives a pairing to an end by each route,
//!   and `verify` is the grammar every stream must fit; its own tests show that it refuses the
//!   streams of the bugs this work fixed (a dial that fails leaves `connecting` as the last event).
//! - **A cancel is a cancel.** `pair.cancel` works from any connection, as `pair.answer` does. The
//!   connection that started the pairing and every `pairing` subscriber hear the end once, and the
//!   event says whose cancel it was (`cancelled_by`). A connection that started a pairing and did
//!   not subscribe keeps M1's stream: its own events on its own connection, and the error that
//!   answers a `pair.start` that fails, with no event of its own after the error.
//!
//! Nothing here touches a compositor: the daemons are in-process, on loopback, with no input.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use ferry_core::api::UiView;
use ferry_net::{Endpoint, EndpointConfig, Identity, PairingWindow, TrustSet};
use ferryd::Daemon;
use serde_json::{Value, json};
use support::*;

const LONG: Duration = Duration::from_secs(10);

// --- the grammar -----------------------------------------------------------------------------

/// Whether `ev` is the last event of the pairing it belongs to, for a pairing run as `role`.
fn is_last(role: &str, ev: &Value) -> bool {
    match ev["stage"].as_str().unwrap() {
        "paired" | "closed" => true,
        "failed" => role == "dialer",
        _ => false,
    }
}

/// Why `events`, the `pairing` events one listener heard of one pairing run as `role`, are not a
/// whole pairing: exactly one last event, at the end; every end says why; every cancel says
/// whose; an acceptor's attempt that fails is followed by the window staying open or closing.
fn verify(role: &str, events: &[Value]) -> Result<(), String> {
    let lasts: Vec<usize> = (0..events.len())
        .filter(|&i| is_last(role, &events[i]))
        .collect();
    match lasts.as_slice() {
        [] => return Err("no last event: the pairing never ends".into()),
        [i] if *i + 1 == events.len() => {}
        [i] => {
            return Err(format!(
                "{} events after the last one",
                events.len() - i - 1
            ));
        }
        more => return Err(format!("{} last events", more.len())),
    }
    for (i, ev) in events.iter().enumerate() {
        let stage = ev["stage"].as_str().unwrap();
        if ev["role"] != role {
            return Err(format!("event {i} is not the {role}'s: {ev}"));
        }
        let text = |field: &str| {
            ev.get(field)
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
        };
        match stage {
            "failed" | "closed" => {
                let (Some(reason), Some(_)) = (text("reason"), text("message")) else {
                    return Err(format!("{stage} without a reason and a message: {ev}"));
                };
                // a cancel says whose it was, and only a cancel does
                let by = ev.get("cancelled_by").and_then(Value::as_str);
                if (reason == "cancelled") != by.is_some_and(|b| b == "local" || b == "peer") {
                    return Err(format!("`cancelled_by` and the reason disagree: {ev}"));
                }
            }
            "listening" | "connecting" | "code" | "waiting_peer" => {
                if ev.get("reason").is_some() || ev.get("cancelled_by").is_some() {
                    return Err(format!("{stage} has an ending's fields: {ev}"));
                }
            }
            "paired" => {}
            other => return Err(format!("a stage that does not exist: {other}")),
        }
        if role == "dialer" && matches!(stage, "listening" | "closed") {
            return Err(format!("a dialer has no window: {ev}"));
        }
        if role == "acceptor" && stage == "connecting" {
            return Err(format!("an acceptor does not dial: {ev}"));
        }
        // the window of an acceptor goes on after a failed attempt, or ends
        if role == "acceptor" && stage == "failed" && i + 1 < events.len() {
            let next = events[i + 1]["stage"].as_str().unwrap();
            if !matches!(next, "listening" | "closed") {
                return Err(format!("an attempt failed and {next} followed"));
            }
        }
    }
    Ok(())
}

fn event(role: &str, stage: &str, reason: Option<&str>) -> Value {
    let mut ev = json!({"stage": stage, "role": role, "expires_ms": 1000});
    if let Some(reason) = reason {
        ev["reason"] = json!(reason);
        ev["message"] = json!("text");
        if reason == "cancelled" {
            ev["cancelled_by"] = json!("local");
        }
    }
    ev
}

fn run(role: &str, stages: &[(&str, Option<&str>)]) -> Vec<Value> {
    stages.iter().map(|(s, r)| event(role, s, *r)).collect()
}

/// A stream that is a whole pairing.
fn whole(role: &str, stages: &[(&str, Option<&str>)]) -> Vec<Value> {
    let events = run(role, stages);
    assert_eq!(verify(role, &events), Ok(()), "{events:?}");
    events
}

#[test]
fn the_grammar_takes_the_whole_pairings_and_refuses_the_ones_that_were_bugs() {
    // whole pairings, as the tests below produce them
    whole(
        "dialer",
        &[("connecting", None), ("failed", Some("unreachable"))],
    );
    whole(
        "dialer",
        &[
            ("connecting", None),
            ("code", None),
            ("waiting_peer", None),
            ("paired", None),
        ],
    );
    whole(
        "acceptor",
        &[
            ("listening", None),
            ("code", None),
            ("failed", Some("rejected")),
            ("listening", None),
            ("closed", Some("cancelled")),
        ],
    );
    whole(
        "acceptor",
        &[("listening", None), ("code", None), ("paired", None)],
    );

    // a dial that fails and leaves `connecting` as the last event (the bug of the failed dial)
    let hung = run("dialer", &[("connecting", None)]);
    assert!(verify("dialer", &hung).unwrap_err().contains("never ends"));
    // an acceptor whose last attempt failed and nothing said the window ended (the save_failed bug)
    let open = run(
        "acceptor",
        &[
            ("listening", None),
            ("code", None),
            ("failed", Some("save_failed")),
        ],
    );
    assert!(
        verify("acceptor", &open)
            .unwrap_err()
            .contains("never ends")
    );
    // two last events, or one and then more
    let twice = run(
        "dialer",
        &[
            ("connecting", None),
            ("failed", Some("x")),
            ("failed", Some("x")),
        ],
    );
    assert!(verify("dialer", &twice).unwrap_err().contains("2 last"));
    let after = run("dialer", &[("failed", Some("x")), ("code", None)]);
    assert!(
        verify("dialer", &after)
            .unwrap_err()
            .contains("after the last")
    );
    // an acceptor's attempt that fails, and then a code with no `listening` between
    let skipped = run(
        "acceptor",
        &[
            ("listening", None),
            ("failed", Some("x")),
            ("code", None),
            ("paired", None),
        ],
    );
    assert!(
        verify("acceptor", &skipped)
            .unwrap_err()
            .contains("followed")
    );
    // an end with no reason, a cancel with no owner, an owner with no cancel
    let mut bare = whole("dialer", &[("connecting", None), ("failed", Some("x"))]);
    bare[1].as_object_mut().unwrap().remove("reason");
    assert!(
        verify("dialer", &bare)
            .unwrap_err()
            .contains("without a reason")
    );
    let mut unowned = whole(
        "dialer",
        &[("connecting", None), ("failed", Some("cancelled"))],
    );
    unowned[1].as_object_mut().unwrap().remove("cancelled_by");
    assert!(verify("dialer", &unowned).unwrap_err().contains("disagree"));
    let mut stray = whole(
        "dialer",
        &[("connecting", None), ("failed", Some("rejected"))],
    );
    stray[1]["cancelled_by"] = json!("peer");
    assert!(verify("dialer", &stray).unwrap_err().contains("disagree"));
    // another machine's role
    let mixed = [
        &run("acceptor", &[("listening", None)])[..],
        &whole("dialer", &[("paired", None)])[..],
    ]
    .concat();
    assert!(
        verify("dialer", &mixed)
            .unwrap_err()
            .contains("not the dialer's")
    );
}

// --- the rig ---------------------------------------------------------------------------------

/// What one listener heard: a connection subscribed to `pairing` (and `ui`, for the sentinel)
/// from before the pairing began.
struct Watch {
    c: Client,
    /// Every pairing event heard, oldest first.
    seen: Vec<Value>,
    /// How many of them the test has taken (`next` hands out `seen[read..]` before it reads on).
    read: usize,
}

impl Watch {
    fn new(d: &Daemon) -> Self {
        let mut c = Client::connect(d.socket_path());
        c.subscribe(&["pairing", "ui"]);
        Self {
            c,
            seen: Vec::new(),
            read: 0,
        }
    }

    fn stages(&self) -> Vec<String> {
        stages(&self.seen)
    }

    /// The next pairing event the test has not taken.
    fn next(&mut self) -> Value {
        while self.read >= self.seen.len() {
            let line = self
                .c
                .read_within(LONG)
                .unwrap_or_else(|| panic!("no pairing event; heard {:?}", self.stages()));
            if line["event"] == "pairing" {
                self.seen.push(line["data"].clone());
            }
        }
        self.read += 1;
        self.seen[self.read - 1].clone()
    }

    /// Events until one at `stage`, which is returned. A pairing that ends first is a failure of
    /// the test, unless that is what it waits for.
    fn until(&mut self, stage: &str) -> Value {
        loop {
            let ev = self.next();
            if ev["stage"] == stage {
                return ev;
            }
            let ends = matches!(ev["stage"].as_str(), Some("failed" | "closed" | "paired"));
            assert!(!ends, "waiting for {stage}; heard {:?}", self.stages());
        }
    }

    /// Events until the last one of a pairing run as `role`.
    fn until_last(&mut self, role: &str) -> Value {
        loop {
            let ev = self.next();
            if is_last(role, &ev) {
                return ev;
            }
        }
    }

    /// The whole pairing, checked: a `ui` event published after the last event is a sentinel on
    /// the same queue, so every event published before it has come when it does. Then a quiet
    /// moment for what the sentinel cannot order (a note from the net thread).
    fn finish(mut self, d: &Daemon, role: &str) -> Vec<Value> {
        d.publish_ui(UiView::Pair);
        loop {
            let line = self.c.read_within(LONG).expect("the sentinel never came");
            match line["event"].as_str() {
                Some("ui") => break,
                Some("pairing") => self.seen.push(line["data"].clone()),
                _ => {}
            }
        }
        for line in self.c.lines_until_quiet(Duration::from_millis(150)) {
            if line["event"] == "pairing" {
                self.seen.push(line["data"].clone());
            }
        }
        if let Err(why) = verify(role, &self.seen) {
            panic!("{why}; heard {:?}", self.stages());
        }
        self.seen
    }
}

fn stages(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e.get("reason").and_then(Value::as_str) {
            Some(r) => format!("{}({r})", e["stage"].as_str().unwrap()),
            None => e["stage"].as_str().unwrap().to_string(),
        })
        .collect()
}

/// The connection that started a pairing and did not subscribe: M1's opener. The `pairing` events
/// of its pairing come on its connection, between the answers to its requests.
struct Opener {
    c: Client,
    heard: Vec<Value>,
}

impl Opener {
    fn new(d: &Daemon) -> Self {
        Self {
            c: Client::connect(d.socket_path()),
            heard: Vec::new(),
        }
    }

    fn take(&mut self, events: Vec<Value>) {
        self.heard.extend(
            events
                .into_iter()
                .filter(|e| e["event"] == "pairing")
                .map(|e| e["data"].clone()),
        );
    }

    /// The whole response line; the events before it are kept.
    fn call(&mut self, method: &str, params: Value) -> Value {
        let (response, events) = self.c.call_collecting(method, params);
        self.take(events);
        response
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let response = self.call(method, params);
        assert!(response.get("error").is_none(), "{method}: {response}");
        response["result"].clone()
    }

    /// Reads until `quiet` passes with no line, keeping the pairing events.
    fn drain(&mut self, quiet: Duration) {
        let lines = self.c.lines_until_quiet(quiet);
        self.take(lines);
    }

    /// Reads until the last event of a pairing run as `role`.
    fn until_last(&mut self, role: &str) -> Value {
        loop {
            if let Some(ev) = self.heard.iter().find(|e| is_last(role, e)) {
                return ev.clone();
            }
            let line = self.c.read_within(LONG).unwrap_or_else(|| {
                panic!(
                    "the opener never heard the end; heard {:?}",
                    stages(&self.heard)
                )
            });
            self.take(vec![line]);
        }
    }
}

struct Duo {
    da: Daemon,
    db: Daemon,
    ea: Env,
    eb: Env,
}

impl Duo {
    /// A is `desktop`, B is `laptop`.
    fn new() -> Self {
        let (ea, eb) = (Env::new(), Env::new());
        let mut oa = options(&ea);
        oa.name = Some("desktop".into());
        let mut ob = options(&eb);
        ob.name = Some("laptop".into());
        let (da, db) = (Daemon::start(oa).unwrap(), Daemon::start(ob).unwrap());
        Self { da, db, ea, eb }
    }

    fn b_addr(&self) -> String {
        self.db.net_addr().to_string()
    }

    /// A pin cannot be written there: the file is a directory (a rename over it fails, even for
    /// root), so the pairing that agrees fails at `save_failed`.
    fn break_the_pin_file(env: &Env) {
        std::fs::create_dir_all(env.paths().trusted_file()).unwrap();
    }

    fn shutdown(self) {
        self.da.shutdown();
        self.db.shutdown();
    }
}

/// Both machines, each with a watcher and an opener that did not subscribe.
struct Pair {
    duo: Duo,
    a: Opener,
    b: Opener,
    wa: Watch,
    wb: Watch,
}

impl Pair {
    fn new() -> Self {
        let duo = Duo::new();
        let (a, b) = (Opener::new(&duo.da), Opener::new(&duo.db));
        let (wa, wb) = (Watch::new(&duo.da), Watch::new(&duo.db));
        Self { duo, a, b, wa, wb }
    }

    /// B's window is open, owned by `b`, and A dials it: both are at the code.
    fn reach_the_code(&mut self) {
        self.b.ok("pair.accept", json!(null));
        self.wb.until("listening");
        self.a.ok("pair.start", json!({"addr": self.duo.b_addr()}));
        self.wa.until("code");
        self.wb.until("code");
    }

    /// A connection that started nothing, on A or on B.
    fn panel_a(&self) -> Client {
        Client::connect(self.duo.da.socket_path())
    }

    fn panel_b(&self) -> Client {
        Client::connect(self.duo.db.socket_path())
    }
}

fn cancelled_by(ev: &Value) -> &str {
    ev["cancelled_by"].as_str().unwrap_or("-")
}

fn reason_and_owner(ev: &Value) -> (Option<&str>, &str) {
    (ev["reason"].as_str(), cancelled_by(ev))
}

// --- the dialer ------------------------------------------------------------------------------

/// B has no window: the dial is refused. The opener that did not subscribe has M1's stream, the
/// `connecting` and then the error, and no event after it; a subscriber hears the end.
#[test]
fn a_dial_that_is_refused_ends_with_a_failed_for_the_subscribers_and_the_error_for_the_opener() {
    let duo = Duo::new();
    let mut w = Watch::new(&duo.da);
    let mut a = Opener::new(&duo.da);
    let answer = a.call("pair.start", json!({"addr": duo.b_addr()}));
    assert_eq!(answer["error"]["code"], "pairing_not_accepting", "{answer}");
    assert_eq!(
        stages(&a.heard),
        ["connecting"],
        "M1's stream: the connecting, then the error"
    );
    let last = w.until_last("dialer");
    assert_eq!(reason_and_owner(&last), (Some("not_accepting"), "-"));
    assert_eq!(last["stage"], "failed");
    assert!(
        last["message"]
            .as_str()
            .unwrap()
            .contains("is not accepting pairing")
    );
    assert_eq!(w.finish(&duo.da, "dialer").len(), 2);
    // the opener has the error and nothing after it
    a.drain(Duration::from_millis(250));
    assert_eq!(stages(&a.heard), ["connecting"]);
    // and nothing runs any more
    assert!(a.ok("status", json!(null)).get("pairing").is_none());
    duo.shutdown();
}

/// The same refusal for an opener that subscribed (the Pair panel): the error answers its request,
/// and the end is an event like for any subscriber, once.
#[test]
fn an_opener_that_subscribed_hears_a_failed_dial_end_once() {
    let duo = Duo::new();
    let mut w = Watch::new(&duo.da);
    let (answer, early) =
        w.c.call_collecting("pair.start", json!({"addr": duo.b_addr()}));
    assert_eq!(answer["error"]["code"], "pairing_not_accepting", "{answer}");
    // what came before the answer is heard, and the rest follows it
    w.seen.extend(
        early
            .iter()
            .filter(|e| e["event"] == "pairing")
            .map(|e| e["data"].clone()),
    );
    w.until_last("dialer");
    let seen = w.finish(&duo.da, "dialer");
    assert_eq!(stages(&seen), ["connecting", "failed(not_accepting)"]);
    duo.shutdown();
}

/// Nobody answers: the dial times out (2 s), and that is the end for a subscriber. Another
/// connection cancels meanwhile in a second run, so that end reads as the cancel it was.
#[test]
fn a_dial_nobody_answers_ends_unreachable_and_a_cancel_meanwhile_ends_it_cancelled() {
    let duo = Duo::new();
    // a socket that takes the packets and never answers
    let hole = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = hole.local_addr().unwrap().to_string();

    // no cancel: unreachable
    let mut w = Watch::new(&duo.da);
    let mut a = Opener::new(&duo.da);
    let answer = a.call("pair.start", json!({ "addr": addr }));
    assert_eq!(answer["error"]["code"], "peer_unreachable", "{answer}");
    let last = w.until_last("dialer");
    assert_eq!(reason_and_owner(&last), (Some("unreachable"), "-"));
    assert!(last["message"].as_str().unwrap().contains("did not answer"));
    w.finish(&duo.da, "dialer");

    // a cancel from another connection while the dial runs: the dial still takes its time, and
    // then the pairing ends as the cancel
    let mut w = Watch::new(&duo.da);
    let opener = std::thread::spawn({
        let (path, addr) = (duo.da.socket_path().to_path_buf(), addr.clone());
        move || {
            Client::connect(&path)
                .call_collecting("pair.start", json!({ "addr": addr }))
                .0
        }
    });
    w.until("connecting");
    Client::connect(duo.da.socket_path()).result("pair.cancel", json!(null));
    let last = w.until_last("dialer");
    assert_eq!(
        reason_and_owner(&last),
        (Some("cancelled"), "local"),
        "{last}"
    );
    // the opener's request was still answered with what the dial found
    assert_eq!(opener.join().unwrap()["error"]["code"], "peer_unreachable");
    w.finish(&duo.da, "dialer");
    drop(hole);
    duo.shutdown();
}

/// The peer takes the connection and drops it at once. Whether the dial fails or the attempt does,
/// the pairing ends once.
#[test]
fn a_peer_that_hangs_up_on_the_dial_ends_the_pairing_once() {
    let duo = Duo::new();
    let rt = rt();
    let ep = {
        let _g = rt.enter();
        Endpoint::bind(
            Identity::ephemeral().unwrap(),
            TrustSet::new(),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                ..EndpointConfig::default()
            },
        )
        .unwrap()
    };
    ep.set_pairing(Some(PairingWindow {
        until: Instant::now() + Duration::from_secs(60),
    }));
    let addr = ep.local_addr().unwrap().to_string();
    rt.spawn({
        let ep = ep.clone();
        async move {
            while let Some(pc) = ep.accept_pairing().await {
                drop(pc);
            }
        }
    });
    let mut w = Watch::new(&duo.da);
    let mut a = Opener::new(&duo.da);
    let _ = a.call("pair.start", json!({ "addr": addr }));
    let last = w.until_last("dialer");
    assert!(
        matches!(
            last["reason"].as_str(),
            Some("connect_failed" | "lost" | "unreachable")
        ),
        "{last}"
    );
    w.finish(&duo.da, "dialer");
    duo.shutdown();
}

#[test]
fn the_codes_differ_and_the_dialer_says_so() {
    let mut p = Pair::new();
    p.reach_the_code();
    p.a.ok("pair.answer", json!({"match": false}));
    let last = p.wa.until_last("dialer");
    assert_eq!(
        (last["stage"].as_str(), last["reason"].as_str()),
        (Some("failed"), Some("rejected"))
    );
    assert_eq!(last["cancelled_by"], Value::Null, "a no is not a cancel");
    p.wa.finish(&p.duo.da, "dialer");
    // B's attempt failed and its window stays open
    p.wb.until("failed");
    p.wb.until("listening");
    p.duo.shutdown();
}

#[test]
fn the_other_machine_says_no_and_the_dialer_fails() {
    let mut p = Pair::new();
    p.reach_the_code();
    p.b.ok("pair.answer", json!({"match": false}));
    let last = p.wa.until_last("dialer");
    assert_eq!(
        (last["stage"].as_str(), last["reason"].as_str()),
        (Some("failed"), Some("rejected"))
    );
    assert_eq!(last["cancelled_by"], Value::Null, "a no is not a cancel");
    p.wa.finish(&p.duo.da, "dialer");
    p.duo.shutdown();
}

#[test]
fn both_say_yes_and_the_dialer_is_paired_once() {
    let mut p = Pair::new();
    p.reach_the_code();
    p.a.ok("pair.answer", json!({"match": true}));
    p.wa.until("waiting_peer");
    p.b.ok("pair.answer", json!({"match": true}));
    assert_eq!(p.wa.until_last("dialer")["stage"], "paired");
    let seen = p.wa.finish(&p.duo.da, "dialer");
    assert_eq!(seen.first().unwrap()["stage"], "connecting");
    // the opener's own stream, which never subscribed, is a whole pairing too
    p.a.until_last("dialer");
    p.a.drain(Duration::from_millis(150));
    assert_eq!(
        verify("dialer", &p.a.heard),
        Ok(()),
        "{:?}",
        stages(&p.a.heard)
    );
    assert_eq!(p.wb.until_last("acceptor")["stage"], "paired");
    p.wb.finish(&p.duo.db, "acceptor");
    p.duo.shutdown();
}

/// The pin cannot be written on the dialer: it fails with `save_failed`, once.
#[test]
fn a_pin_that_cannot_be_written_ends_the_dialer_with_save_failed() {
    let mut p = Pair::new();
    Duo::break_the_pin_file(&p.duo.ea);
    p.reach_the_code();
    p.a.ok("pair.answer", json!({"match": true}));
    p.wa.until("waiting_peer");
    p.b.ok("pair.answer", json!({"match": true}));
    let last = p.wa.until_last("dialer");
    assert_eq!(
        (last["stage"].as_str(), last["reason"].as_str()),
        (Some("failed"), Some("save_failed"))
    );
    p.wa.finish(&p.duo.da, "dialer");
    p.duo.shutdown();
}

// --- cancels, from every connection -----------------------------------------------------------

/// The opener cancels on its own connection (what M1 allowed).
#[test]
fn the_opener_cancels_a_dialer_at_the_code() {
    let mut p = Pair::new();
    p.reach_the_code();
    p.a.ok("pair.cancel", json!(null));
    let last = p.wa.until_last("dialer");
    assert_eq!(
        reason_and_owner(&last),
        (Some("cancelled"), "local"),
        "{last}"
    );
    p.wa.finish(&p.duo.da, "dialer");
    // the other machine heard it as the other peer's cancel
    let theirs = p.wb.until("failed");
    assert_eq!(reason_and_owner(&theirs), (Some("cancelled"), "peer"));
    p.duo.shutdown();
}

/// Another connection cancels (the panel). The opener, which did not subscribe, hears the end
/// on its own connection, once; the subscriber hears it once.
#[test]
fn another_connection_cancels_a_dialer_and_the_opener_that_did_not_subscribe_hears_it_once() {
    let mut p = Pair::new();
    p.reach_the_code();
    let mut panel = p.panel_a();
    let (answer, _) = panel.call_collecting("pair.cancel", json!(null));
    assert_eq!(answer["result"], json!({}), "{answer}");
    // the opener: its events, on its own connection, and no second copy of any
    let end = p.a.until_last("dialer");
    assert_eq!(
        (end["stage"].as_str(), reason_and_owner(&end)),
        (Some("failed"), (Some("cancelled"), "local"))
    );
    p.a.drain(Duration::from_millis(250));
    assert_eq!(
        stages(&p.a.heard),
        ["connecting", "code", "failed(cancelled)"]
    );
    assert_eq!(verify("dialer", &p.a.heard), Ok(()));
    p.wa.until_last("dialer");
    p.wa.finish(&p.duo.da, "dialer");
    p.duo.shutdown();
}

/// The connection that started the pairing closes (Ctrl+C): that is a cancel, heard by the
/// subscribers.
#[test]
fn the_opener_closing_its_connection_ends_the_pairing_as_a_local_cancel() {
    let mut p = Pair::new();
    p.reach_the_code();
    let a = std::mem::replace(&mut p.a, Opener::new(&p.duo.da));
    drop(a);
    let last = p.wa.until_last("dialer");
    assert_eq!(reason_and_owner(&last), (Some("cancelled"), "local"));
    p.wa.finish(&p.duo.da, "dialer");
    p.duo.shutdown();
}

#[test]
fn a_cancel_with_no_pairing_is_no_pairing_on_any_connection() {
    let duo = Duo::new();
    let mut x = Client::connect(duo.da.socket_path());
    let (code, message, _) = x.error("pair.cancel", json!(null));
    assert_eq!(code, "no_pairing", "{message}");
    assert!(
        message.contains("no pairing runs on this machine"),
        "{message}"
    );
    duo.shutdown();
}

// --- the acceptor ----------------------------------------------------------------------------

/// The opener closes the window.
#[test]
fn the_opener_cancels_the_window_and_the_subscribers_hear_it_close() {
    let duo = Duo::new();
    let mut b = Opener::new(&duo.db);
    let mut w = Watch::new(&duo.db);
    b.ok("pair.accept", json!(null));
    w.until("listening");
    assert_eq!(b.ok("pair.cancel", json!(null)), json!({}));
    // M1's opener: the closing, on its own connection (before or after the answer)
    let end = b.until_last("acceptor");
    assert_eq!(end["stage"], "closed");
    let last = w.until_last("acceptor");
    assert_eq!(
        (last["stage"].as_str(), reason_and_owner(&last)),
        (Some("closed"), (Some("cancelled"), "local"))
    );
    w.finish(&duo.db, "acceptor");
    duo.shutdown();
}

/// Another connection closes the window: the opener (M1, unsubscribed) hears the closing on its
/// own connection, once, and nothing else.
#[test]
fn another_connection_closes_the_window_and_the_opener_that_did_not_subscribe_hears_it() {
    let duo = Duo::new();
    let mut b = Opener::new(&duo.db);
    let mut w = Watch::new(&duo.db);
    // M1: the opener has the answer and no `listening`
    assert_eq!(b.ok("pair.accept", json!(null))["expires_ms"], 120_000);
    assert!(b.heard.is_empty(), "{:?}", b.heard);
    w.until("listening");
    let mut panel = Client::connect(duo.db.socket_path());
    panel.result("pair.cancel", json!(null));
    let end = b.until_last("acceptor");
    assert_eq!(end["stage"], "closed");
    assert_eq!(reason_and_owner(&end), (Some("cancelled"), "local"));
    b.drain(Duration::from_millis(250));
    assert_eq!(
        stages(&b.heard),
        ["closed(cancelled)"],
        "one end, and nothing else"
    );
    // the window is gone for everyone
    assert!(panel.result("status", json!(null)).get("pairing").is_none());
    w.until_last("acceptor");
    w.finish(&duo.db, "acceptor");
    duo.shutdown();
}

#[test]
fn the_opener_closing_its_connection_closes_the_window() {
    let duo = Duo::new();
    let mut b = Opener::new(&duo.db);
    let mut w = Watch::new(&duo.db);
    b.ok("pair.accept", json!(null));
    w.until("listening");
    drop(b);
    let last = w.until_last("acceptor");
    assert_eq!(
        (last["stage"].as_str(), cancelled_by(&last)),
        (Some("closed"), "local")
    );
    w.finish(&duo.db, "acceptor");
    duo.shutdown();
}

/// A cancel in the middle of an attempt: the attempt fails as a local cancel, the window with it,
/// and the dialer hears the other peer's cancel.
#[test]
fn another_connection_cancels_an_acceptor_at_the_code() {
    let mut p = Pair::new();
    p.reach_the_code();
    p.panel_b().result("pair.cancel", json!(null));
    let failed = p.wb.until("failed");
    assert_eq!(reason_and_owner(&failed), (Some("cancelled"), "local"));
    let closed = p.wb.until("closed");
    assert_eq!(reason_and_owner(&closed), (Some("cancelled"), "local"));
    p.wb.finish(&p.duo.db, "acceptor");
    // the opener (unsubscribed) heard its events and the end, each once
    p.b.until_last("acceptor");
    p.b.drain(Duration::from_millis(250));
    assert_eq!(
        stages(&p.b.heard),
        ["code", "failed(cancelled)", "closed(cancelled)"]
    );
    assert_eq!(verify("acceptor", &p.b.heard), Ok(()));
    // and the dialer: the other peer cancelled
    let theirs = p.wa.until_last("dialer");
    assert_eq!(reason_and_owner(&theirs), (Some("cancelled"), "peer"));
    p.wa.finish(&p.duo.da, "dialer");
    p.duo.shutdown();
}

/// The other machine cancels an attempt: the window stays open (`failed`, then `listening`), the
/// event names the other peer, and a cancel here closes the window afterwards.
#[test]
fn a_cancel_by_the_dialer_fails_the_attempt_and_the_window_stays_until_it_is_closed() {
    let mut p = Pair::new();
    p.reach_the_code();
    p.a.ok("pair.cancel", json!(null));
    let failed = p.wb.until("failed");
    assert_eq!(reason_and_owner(&failed), (Some("cancelled"), "peer"));
    p.wb.until("listening");
    p.panel_b().result("pair.cancel", json!(null));
    let closed = p.wb.until("closed");
    assert_eq!(reason_and_owner(&closed), (Some("cancelled"), "local"));
    p.wb.finish(&p.duo.db, "acceptor");
    p.wa.until_last("dialer");
    p.wa.finish(&p.duo.da, "dialer");
    p.duo.shutdown();
}

/// Three failed attempts close the window by themselves.
#[test]
fn three_failed_attempts_close_the_window_and_it_ends_once() {
    let mut p = Pair::new();
    p.b.ok("pair.accept", json!(null));
    p.wb.until("listening");
    for attempt in 1..=3 {
        p.a.ok("pair.start", json!({"addr": p.duo.b_addr()}));
        p.wa.until("code");
        p.wb.until("code");
        p.a.ok("pair.answer", json!({"match": false}));
        p.wa.until_last("dialer");
        p.wb.until("failed");
        if attempt < 3 {
            p.wb.until("listening");
        }
    }
    let closed = p.wb.until("closed");
    assert_eq!(closed["reason"], "failures");
    p.wb.finish(&p.duo.db, "acceptor");
    p.duo.shutdown();
}

#[test]
fn both_say_yes_and_the_acceptor_is_paired_once() {
    let mut p = Pair::new();
    p.reach_the_code();
    p.a.ok("pair.answer", json!({"match": true}));
    p.wa.until("waiting_peer");
    p.b.ok("pair.answer", json!({"match": true}));
    assert_eq!(p.wb.until_last("acceptor")["stage"], "paired");
    p.wb.finish(&p.duo.db, "acceptor");
    p.wa.until_last("dialer");
    p.wa.finish(&p.duo.da, "dialer");
    p.duo.shutdown();
}

/// The pin cannot be written on the acceptor: the attempt fails with `save_failed`, and the window
/// is closed with it (net ended it when it sent the pin), so the acceptor's pairing ends once.
#[test]
fn a_pin_that_cannot_be_written_ends_the_acceptor_with_a_failed_and_a_closed() {
    let mut p = Pair::new();
    Duo::break_the_pin_file(&p.duo.eb);
    p.reach_the_code();
    p.a.ok("pair.answer", json!({"match": true}));
    p.wa.until("waiting_peer");
    p.b.ok("pair.answer", json!({"match": true}));
    let failed = p.wb.until("failed");
    assert_eq!(failed["reason"], "save_failed");
    let closed = p.wb.until("closed");
    assert_eq!(closed["reason"], "save_failed");
    p.wb.finish(&p.duo.db, "acceptor");
    p.duo.shutdown();
}

/// A subscriber that comes in the middle is told the pairing as it is, and hears its end once.
#[test]
fn a_subscriber_that_joins_late_hears_the_end_once() {
    let mut p = Pair::new();
    p.reach_the_code();
    let mut late = Watch::new(&p.duo.db);
    assert_eq!(late.next()["stage"], "code");
    p.panel_b().result("pair.cancel", json!(null));
    late.until("failed");
    late.until("closed");
    late.finish(&p.duo.db, "acceptor");
    p.duo.shutdown();
}
