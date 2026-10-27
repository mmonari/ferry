//! Toasts through the hooks in `ctl`: real daemons on loopback, and a
//! [`Recorder`] for each one's desktop. The code of a pairing request, its result,
//! A lost and a returning link, the other side unpairing this machine,
//! A lost layout edit and the `attention` entries.
//!
//! **Every toast is also a `notice` event.** Each test has a socket client subscribed to
//! the `notice` topic and checks that it sees the toast the desktop got, word for word; the
//! button of the code toast reaches a `ui` subscriber.
//!
//! The notify task alone is tested in `src/notify/tests.rs`; the test that reaches a D-Bus is
//! `notify_bus.rs`. No test here touches a bus.
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

use ferry_core::api::LinkState;
use ferry_core::geom::{Layout, Origin, Revision};
use ferry_core::{Micros, PeerId};
use ferryd::chan::{CtlMsg, LinkUpdate};
use ferryd::notify::{Call, Limits, Mode, Options, Recorder, Urgency};
use ferryd::{Daemon, FakePortal, Options as DaemonOptions};
use serde_json::{Value, json};
use support::*;

/// Options for a daemon whose desktop is `recorder`, with a connection window of
/// `connection_ms`. The key lives in the env, so a restart is the same peer.
fn toasting(env: &Env, name: &str, recorder: &Recorder, connection_ms: u64) -> DaemonOptions {
    let mut o = options(env);
    o.name = Some(name.into());
    o.identity = None;
    o.toasts = Options {
        mode: Mode::Recorder(recorder.clone()),
        limits: Limits {
            connection: Micros::from_millis(connection_ms),
            ..Limits::default()
        },
    };
    o
}

/// Polls `f` until it returns `Some` (up to 20 s: a handshake on a loaded host is slow).
fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn summaries_include(rec: &Recorder, summary: &str) -> bool {
    rec.summaries().iter().any(|s| s == summary)
}

/// A socket client subscribed to `topic` of `d`.
fn watch(d: &Daemon, topic: &str) -> Client {
    let mut c = Client::connect(d.socket_path());
    c.subscribe(&[topic]);
    c
}

/// The data of the next `notice` event (up to 20 s).
fn notice(c: &mut Client) -> Value {
    c.event_within("notice", Duration::from_secs(20))
        .expect("no notice event within 20 s")["data"]
        .clone()
}

/// `(kind, title, urgency)` of a notice: what a frontend reads first.
fn gist(n: &Value) -> (&str, &str, &str) {
    (
        n["kind"].as_str().unwrap(),
        n["title"].as_str().unwrap(),
        n["urgency"].as_str().unwrap(),
    )
}

/// A socket client for a pairing: the responses to its requests, and the `pairing` events that
/// come between them.
struct Pairing {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    events: VecDeque<Value>,
}

impl Pairing {
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

    /// The result of one request; events that come first are kept.
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut req =
            serde_json::to_vec(&json!({"id": id, "method": method, "params": params})).unwrap();
        req.push(b'\n');
        self.writer.write_all(&req).unwrap();
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no answer to {method}");
            let Some(v) = self.line(left) else { continue };
            if v.get("event").is_some() {
                self.events.push_back(v);
            } else {
                assert_eq!(v["id"], id, "{v}");
                assert!(v.get("error").is_none(), "{method}: {v}");
                return v["result"].clone();
            }
        }
    }

    /// Events until one at `stage`; returns its data.
    fn until_stage(&mut self, stage: &str) -> Value {
        loop {
            let ev = match self.events.pop_front() {
                Some(v) => v,
                None => self
                    .line(Duration::from_secs(20))
                    .unwrap_or_else(|| panic!("no {stage} event")),
            };
            assert_eq!(ev["event"], "pairing", "{ev}");
            if ev["data"]["stage"] == stage {
                return ev["data"].clone();
            }
        }
    }
}

/// `418207` as the toast writes it.
fn spaced(code: &str) -> String {
    format!("{} {}", &code[..3], &code[3..])
}

/// A listens, B dials, both reach the code, and A answers yes first: A's yes can only move it
/// to "waiting for the peer", so its code toast is closed. Then B says yes and both are paired.
/// Returns A's code.
fn pair(a: &mut Pairing, b: &mut Pairing, a_addr: std::net::SocketAddr) -> String {
    a.ok("pair.accept", json!(null));
    b.ok("pair.start", json!({"addr": a_addr.to_string()}));
    let code = a.until_stage("code")["code"].as_str().unwrap().to_string();
    b.until_stage("code");
    a.ok("pair.answer", json!({"match": true}));
    a.until_stage("waiting_peer");
    b.ok("pair.answer", json!({"match": true}));
    a.until_stage("paired");
    b.until_stage("paired");
    code
}

fn connected(client: &mut Client, fp: &str) -> bool {
    let st = client.result("status", json!(null));
    st["peers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["fingerprint"] == fp && p["state"] == "connected")
}

/// FR-PAIR-13 and J2 steps 3 to 5: the acceptor shows its code in a critical notification with
/// the action that opens the Pair tab; answering here takes it down; both sides get "Paired
/// with"; the dialer started the pairing where it sees the code, so it gets no code toast.
#[test]
fn a_pairing_request_shows_its_code_to_the_acceptor_and_the_result_to_both() {
    let (ea, eb) = (Env::new(), Env::new());
    let (ra, rb) = (Recorder::new(), Recorder::new());
    let da = Daemon::start(toasting(&ea, "desktop", &ra, 30_000)).unwrap();
    let db = Daemon::start(toasting(&eb, "laptop", &rb, 30_000)).unwrap();
    let (mut na, mut nb, mut ui) = (watch(&da, "notice"), watch(&db, "notice"), watch(&da, "ui"));
    let (mut a, mut b) = (
        Pairing::connect(da.socket_path()),
        Pairing::connect(db.socket_path()),
    );
    a.ok("pair.accept", json!(null));
    b.ok("pair.start", json!({"addr": da.net_addr().to_string()}));
    let code = a.until_stage("code")["code"].as_str().unwrap().to_string();
    b.until_stage("code");

    // the code toast, as the desktop got it
    let calls = until("the code toast", || {
        let calls = ra.calls();
        (!calls.is_empty()).then_some(calls)
    });
    let Call::Show {
        id,
        summary,
        body,
        urgency,
        expire_ms,
        action,
        replaces,
    } = &calls[0]
    else {
        panic!("{calls:?}");
    };
    assert_eq!(summary, &format!("Pairing code {}", spaced(&code)));
    assert!(body.starts_with("laptop is asking to pair"), "{body}");
    assert_eq!(*urgency, Urgency::Critical);
    assert_eq!(action.as_deref(), Some("open"));
    assert_eq!(*replaces, None);
    assert!(
        expire_ms.is_some_and(|ms| (1..=120_000).contains(&ms)),
        "{expire_ms:?}"
    );
    // and as a frontend gets it: the same words, a critical `pair_code` about the dialer
    let n = notice(&mut na);
    assert_eq!(gist(&n), ("pair_code", summary.as_str(), "critical"), "{n}");
    assert_eq!(n["body"], body.as_str());
    assert_eq!(n["peer"], "laptop");

    // the user presses the button: a frontend is asked to open the Pair tab
    ra.press(*id, "open");
    let asked = ui
        .event_within("ui", Duration::from_secs(20))
        .expect("no ui event");
    assert_eq!(asked["data"], json!({"open": "pair"}));

    // A answers first, which takes the code toast down; then B answers
    a.ok("pair.answer", json!({"match": true}));
    a.until_stage("waiting_peer");
    b.ok("pair.answer", json!({"match": true}));
    a.until_stage("paired");
    b.until_stage("paired");

    until("A's three calls", || (ra.calls().len() == 3).then_some(()));
    let calls = ra.calls();
    // answering took the code down, and the result is a notification of its own
    assert_eq!(calls[1], Call::Close { id: *id });
    let Call::Show {
        summary,
        urgency,
        replaces,
        ..
    } = &calls[2]
    else {
        panic!("{calls:?}");
    };
    assert_eq!(summary, "Paired with laptop");
    assert_eq!((*urgency, *replaces), (Urgency::Normal, None));
    let n = notice(&mut na);
    assert_eq!(gist(&n), ("paired", "Paired with laptop", "normal"), "{n}");

    // B dialled: it saw the code where it started the pairing, and its toast is the result
    let n = notice(&mut nb);
    assert_eq!(gist(&n), ("paired", "Paired with desktop", "normal"), "{n}");
    until("B's result", || {
        summaries_include(&rb, "Paired with desktop").then_some(())
    });
    assert_eq!(rb.summaries(), ["Paired with desktop"]);
    da.shutdown();
    db.shutdown();
}

/// J2 step 5: the peer says the codes differ while A's code is on screen. The failure takes
/// the code's place, on A; B shows the same words.
#[test]
fn a_rejected_pairing_replaces_the_code_toast_with_the_reason() {
    let (ea, eb) = (Env::new(), Env::new());
    let (ra, rb) = (Recorder::new(), Recorder::new());
    let da = Daemon::start(toasting(&ea, "desktop", &ra, 30_000)).unwrap();
    let db = Daemon::start(toasting(&eb, "laptop", &rb, 30_000)).unwrap();
    let (mut na, mut nb) = (watch(&da, "notice"), watch(&db, "notice"));
    let (mut a, mut b) = (
        Pairing::connect(da.socket_path()),
        Pairing::connect(db.socket_path()),
    );
    a.ok("pair.accept", json!(null));
    b.ok("pair.start", json!({"addr": da.net_addr().to_string()}));
    a.until_stage("code");
    b.until_stage("code");
    b.ok("pair.answer", json!({"match": false}));
    a.until_stage("failed");
    b.until_stage("failed");

    until("A's two calls", || (ra.calls().len() == 2).then_some(()));
    let calls = ra.calls();
    let Call::Show { id, .. } = &calls[0] else {
        panic!("{calls:?}");
    };
    let Call::Show {
        summary,
        body,
        urgency,
        action,
        replaces,
        ..
    } = &calls[1]
    else {
        panic!("{calls:?}");
    };
    assert_eq!(
        (summary.as_str(), body.as_str()),
        ("Pairing cancelled", "Codes differ")
    );
    assert_eq!((*urgency, action.as_deref()), (Urgency::Normal, None));
    assert_eq!(*replaces, Some(*id), "the failure replaced the code toast");

    // a frontend: the code, then the reason
    let n = notice(&mut na);
    assert_eq!(n["kind"], "pair_code");
    let n = notice(&mut na);
    assert_eq!(
        gist(&n),
        ("pair_failed", "Pairing cancelled", "normal"),
        "{n}"
    );
    assert_eq!(n["body"], "Codes differ");

    let n = notice(&mut nb);
    assert_eq!(
        gist(&n),
        ("pair_failed", "Pairing cancelled", "normal"),
        "{n}"
    );
    assert_eq!(n["body"], "Codes differ");
    until("B's reason", || {
        summaries_include(&rb, "Pairing cancelled").then_some(())
    });
    assert_eq!(rb.calls().len(), 1, "no code toast on the dialer");
    da.shutdown();
    db.shutdown();
}

/// J6 steps 3 and 4 with real daemons: the first connect after pairing is silent, a lost link
/// is "Lost laptop", and the return is "laptop is back".
#[test]
fn a_lost_link_and_its_return_are_toasts_but_the_first_connect_is_not() {
    let (ea, eb) = (Env::new(), Env::new());
    let (ra, rb) = (Recorder::new(), Recorder::new());
    // a short window, so the return is not held back for 30 s
    let da = Daemon::start(toasting(&ea, "desktop", &ra, 300)).unwrap();
    let db = Daemon::start(toasting(&eb, "laptop", &rb, 300)).unwrap();
    let fb = db.fingerprint().to_string();
    let mut na = watch(&da, "notice");
    let (mut a, mut b) = (
        Pairing::connect(da.socket_path()),
        Pairing::connect(db.socket_path()),
    );
    pair(&mut a, &mut b, da.net_addr());
    let mut ctl_a = Client::connect(da.socket_path());
    until("the first connect", || {
        connected(&mut ctl_a, &fb).then_some(())
    });

    // B goes away and comes back as the same peer
    db.shutdown();
    until("Lost laptop", || {
        summaries_include(&ra, "Lost laptop").then_some(())
    });
    let rb2 = Recorder::new();
    let db2 = Daemon::start(toasting(&eb, "laptop", &rb2, 300)).unwrap();
    until("laptop is back", || {
        summaries_include(&ra, "laptop is back").then_some(())
    });

    // in order: the code, the pairing result, then the loss and the return. The first connect,
    // which came before the loss, made no toast.
    let summaries = ra.summaries();
    assert!(summaries[0].starts_with("Pairing code "), "{summaries:?}");
    assert_eq!(
        summaries[1..],
        ["Paired with laptop", "Lost laptop", "laptop is back"]
    );
    // a frontend sees the same four, in the same order
    let seen: Vec<_> = (0..4).map(|_| notice(&mut na)).collect();
    let kinds: Vec<_> = seen.iter().map(|n| n["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["pair_code", "paired", "peer_lost", "peer_back"]);
    assert_eq!(gist(&seen[2]), ("peer_lost", "Lost laptop", "normal"));
    assert_eq!(gist(&seen[3]), ("peer_back", "laptop is back", "normal"));
    assert_eq!(seen[3]["peer"], "laptop");

    // and B's own connect to A, after its restart, is silent
    let (mut ctl_b, fa) = (
        Client::connect(db2.socket_path()),
        da.fingerprint().to_string(),
    );
    until("B connected again", || {
        connected(&mut ctl_b, &fa).then_some(())
    });
    assert!(rb2.summaries().is_empty(), "{:?}", rb2.summaries());
    da.shutdown();
    db2.shutdown();
}

/// B unpairs A. A's pin goes too, A says so in a toast, and the Offline that
/// follows is not a "Lost" toast.
#[test]
fn the_other_side_unpairing_removes_the_pin_and_shows_a_toast() {
    let (ea, eb) = (Env::new(), Env::new());
    let (ra, rb) = (Recorder::new(), Recorder::new());
    let da = Daemon::start(toasting(&ea, "desktop", &ra, 300)).unwrap();
    let db = Daemon::start(toasting(&eb, "laptop", &rb, 300)).unwrap();
    let (fa, fb) = (da.fingerprint().to_string(), db.fingerprint().to_string());
    let mut na = watch(&da, "notice");
    let (mut a, mut b) = (
        Pairing::connect(da.socket_path()),
        Pairing::connect(db.socket_path()),
    );
    pair(&mut a, &mut b, da.net_addr());
    let (mut ctl_a, mut ctl_b) = (
        Client::connect(da.socket_path()),
        Client::connect(db.socket_path()),
    );
    until("connected", || {
        (connected(&mut ctl_a, &fb) && connected(&mut ctl_b, &fa)).then_some(())
    });

    ctl_b.result("unpair", json!({"peer": "desktop"}));

    until("A's toast", || {
        summaries_include(&ra, "laptop unpaired this machine").then_some(())
    });
    until("A's pin gone", || {
        ctl_a.result("peers", json!(null))["peers"]
            .as_array()
            .unwrap()
            .is_empty()
            .then_some(())
    });
    let trusted = std::fs::read_to_string(ea.paths().trusted_file()).unwrap_or_default();
    assert!(!trusted.contains(&fb), "A still pins B:\n{trusted}");
    let calls = ra.calls();
    let Some(Call::Show {
        summary,
        body,
        urgency,
        ..
    }) = calls.last()
    else {
        panic!("{calls:?}");
    };
    assert_eq!(summary, "laptop unpaired this machine");
    assert_eq!(body, "Pair again to reconnect");
    assert_eq!(*urgency, Urgency::Normal);
    assert!(
        !summaries_include(&ra, "Lost laptop"),
        "a revoke is not a lost link: {:?}",
        ra.summaries()
    );
    // a frontend: the code, the pairing, and the revoke
    let kinds: Vec<_> = (0..3)
        .map(|_| notice(&mut na)["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["pair_code", "paired", "unpaired"]);
    // B unpaired it itself: it knows, and says nothing
    assert_eq!(rb.summaries(), ["Paired with desktop"]);
    da.shutdown();
    db.shutdown();
}

fn link(fp: &str, state: LinkState) -> CtlMsg {
    CtlMsg::Link(LinkUpdate {
        peer: fp.parse().unwrap(),
        state,
        remote: None,
        rtt_us: None,
        proto: None,
        detail: None,
    })
}

/// FR-UI-09's test: "Flap lap's link 5× in 30 s: one toast". The links are fed to `ctl` as
/// `net` would, for two peers that are pinned and never dialled. `ctl` handles its messages in
/// order, so tower's revoke toast, queued last, proves every link update before it was seen.
#[test]
fn flapping_a_link_five_times_shows_one_toast() {
    let env = Env::new();
    let rec = Recorder::new();
    let d = Daemon::start(toasting(&env, "desk", &rec, 30_000)).unwrap();
    let mut c = Client::connect(d.socket_path());
    let mut n = watch(&d, "notice");
    let (lap, tower) = (fpx(1), fpx(2));
    c.result("trust", json!({"fingerprint": lap, "name": "lap"}));
    c.result("trust", json!({"fingerprint": tower, "name": "tower"}));

    // the first connect is silent, then five times down and up
    assert!(d.send_ctl(link(&lap, LinkState::Connected)));
    for _ in 0..5 {
        assert!(d.send_ctl(link(&lap, LinkState::Offline)));
        assert!(d.send_ctl(link(&lap, LinkState::Connected)));
    }
    assert!(d.send_ctl(CtlMsg::Revoked {
        peer: tower.parse().unwrap()
    }));
    until("the sentinel", || {
        summaries_include(&rec, "tower unpaired this machine").then_some(())
    });
    assert_eq!(rec.summaries(), ["Lost lap", "tower unpaired this machine"]);
    // a frontend sees the same two
    assert_eq!(gist(&notice(&mut n)), ("peer_lost", "Lost lap", "normal"));
    assert_eq!(
        gist(&notice(&mut n)),
        ("unpaired", "tower unpaired this machine", "normal")
    );
    d.shutdown();
}

/// A layout that the fake peer made, at `counter`, with the fake placed at `origin`.
fn layout_by(fake: PeerId, counter: u64, origin: (i32, i32)) -> Layout {
    let mut l = Layout::empty(fake);
    l.rev = Revision {
        counter,
        peer: fake,
    };
    l.set_origin(Origin {
        peer: fake,
        x: origin.0,
        y: origin.1,
    });
    l
}

/// FR-LAYOUT-10 and the `attention` entries, with a daemon whose capture side runs against a
/// stand-in for the portal: a peer that refuses us, a peer nobody placed, and a layout edit
/// that lost. Each is a toast, and the notice a frontend gets says the same.
#[test]
fn a_lost_layout_edit_and_new_attention_entries_are_toasts_and_notices() {
    let env = Env::new();
    let rec = Recorder::new();
    let mut o = toasting(&env, "desk", &rec, 30_000);
    o.fake_portal = Some(FakePortal::new());
    let d = Daemon::start(o).unwrap();
    let mut c = Client::connect(d.socket_path());
    let mut n = watch(&d, "notice");
    let lap = fpx(1);
    c.result("trust", json!({"fingerprint": lap, "name": "lap"}));

    // `peer_refused`: the link of a pinned peer is refused (its key changed). It was never up,
    // so there is no "Lost" toast beside it.
    assert!(d.send_ctl(CtlMsg::Link(LinkUpdate {
        peer: lap.parse().unwrap(),
        state: LinkState::Refused,
        remote: None,
        rtt_us: None,
        proto: Some("2.0".into()),
        detail: Some("version mismatch: 1.0 vs 2.0".into()),
    })));
    let refused = notice(&mut n);
    assert_eq!(
        gist(&refused),
        (
            "attention",
            "lap cannot connect: version mismatch: 1.0 vs 2.0",
            "critical"
        ),
        "{refused}"
    );
    assert_eq!(refused["peer"], "lap");
    assert!(
        refused["body"].as_str().unwrap().contains("ferry doctor"),
        "{refused}"
    );

    // it stays refused, and then goes and comes back: the same problem, not another toast
    assert!(d.send_ctl(link(&lap, LinkState::Offline)));
    assert!(d.send_ctl(link(&lap, LinkState::Refused)));

    // `layout_unplaced`: lap connects, tells its monitors, and nobody placed it
    let lap_id = PeerId::parse(&lap).unwrap();
    assert!(d.send_ctl(CtlMsg::PeerUp {
        peer: lap.parse().unwrap(),
        monitors: vec![mon("eDP-1", 0, 0, 1366, 768)],
    }));
    let unplaced = notice(&mut n);
    assert_eq!(
        gist(&unplaced),
        ("attention", "lap is not placed in the layout", "critical"),
        "{unplaced}"
    );
    assert_eq!(unplaced["body"], "ferry layout place lap --right-of desk");

    // the user places it (an edit of this peer's own), and lap, which never held the edit,
    // pushes a higher revision: the edit lost
    c.result(
        "layout.place",
        json!({"peer": "lap", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
    );
    assert!(d.send_ctl(CtlMsg::PeerLayout {
        peer: lap.parse().unwrap(),
        layout: layout_by(lap_id, 5, (0, 1080)),
    }));
    let replaced = notice(&mut n);
    assert_eq!(
        gist(&replaced),
        ("layout_replaced", "Layout replaced by lap", "normal"),
        "{replaced}"
    );
    assert_eq!(replaced["peer"], "lap");

    // the desktop got the same three, and nothing else (the sentinel is the last one)
    assert_eq!(
        rec.summaries(),
        [
            "lap cannot connect: version mismatch: 1.0 vs 2.0",
            "lap is not placed in the layout",
            "Layout replaced by lap",
        ]
    );
    let st = c.result("status", json!(null));
    assert_eq!(st["counters"]["layout_replaced"], 1);
    assert_eq!(st["counters"]["toasts_shown"], 3);
    d.shutdown();
}

/// `config_invalid`: what the loader warned about at start-up is a toast. (A `notice` for it
/// goes out before any frontend can have connected, so it is the desktop's alone; `status`
/// carries the entry for the others.)
#[test]
fn a_config_the_loader_warned_about_is_a_toast_at_start() {
    let env = Env::new();
    let config_file = env.paths().config_file;
    std::fs::create_dir_all(config_file.parent().unwrap()).unwrap();
    std::fs::write(&config_file, "[peer]\nnmae = \"x\"\n").unwrap();
    let rec = Recorder::new();
    let d = Daemon::start(toasting(&env, "desk", &rec, 30_000)).unwrap();
    let calls = until("the toast", || {
        let calls = rec.calls();
        (!calls.is_empty()).then_some(calls)
    });
    let Call::Show {
        summary,
        body,
        urgency,
        action,
        ..
    } = &calls[0]
    else {
        panic!("{calls:?}");
    };
    assert_eq!(summary, "config.toml: unknown key peer.nmae");
    assert!(body.contains("~/.config/ferry/config.toml"), "{body}");
    assert_eq!((*urgency, action.as_deref()), (Urgency::Critical, None));
    let mut c = Client::connect(d.socket_path());
    let st = c.result("status", json!(null));
    assert_eq!(st["attention"][0]["code"], "config_invalid");
    // it stays in the list; it is not a second toast
    assert_eq!(
        c.result("status", json!(null))["counters"]["toasts_shown"],
        1
    );
    d.shutdown();
}

/// With `[notify] enabled = false`, or with no notification daemon, ferryd
/// goes on deciding toasts, and a `notice` subscriber gets each one; the desktop gets none, and
/// the status counts them. `config.set notify.enabled` turns the desktop on again, live.
#[test]
fn the_toasts_are_decided_even_when_the_desktop_gets_none() {
    // off in the config
    let env = Env::new();
    let config_file = env.paths().config_file;
    std::fs::create_dir_all(config_file.parent().unwrap()).unwrap();
    std::fs::write(&config_file, "[notify]\nenabled = false\n").unwrap();
    let rec = Recorder::new();
    let d = Daemon::start(toasting(&env, "desk", &rec, 30_000)).unwrap();
    let mut c = Client::connect(d.socket_path());
    let mut n = watch(&d, "notice");
    for (i, name) in [(1, "lap"), (2, "tower")] {
        c.result("trust", json!({"fingerprint": fpx(i), "name": name}));
        assert!(d.send_ctl(CtlMsg::Revoked {
            peer: fpx(i).parse().unwrap()
        }));
        // the bar plugin still gets it
        let seen = notice(&mut n);
        assert_eq!(seen["title"], format!("{name} unpaired this machine"));
    }
    let counters = until("two toasts decided", || {
        let st = c.result("status", json!(null));
        (st["counters"]["toasts_shown"] == 2).then(|| st["counters"].clone())
    });
    // each one was decided before the desktop could have had it, so nothing waits to be sent
    assert!(rec.calls().is_empty(), "{:?}", rec.calls());
    assert!(counters.get("toasts_undelivered").is_none(), "{counters}");

    // turned on live: the next toast reaches the desktop
    let set = c.result(
        "config.set",
        json!({"key": "notify.enabled", "value": true}),
    );
    assert_eq!(set["applies"], "live");
    c.result("trust", json!({"fingerprint": fpx(3), "name": "mast"}));
    assert!(d.send_ctl(CtlMsg::Revoked {
        peer: fpx(3).parse().unwrap()
    }));
    until("the desktop's toast", || {
        summaries_include(&rec, "mast unpaired this machine").then_some(())
    });
    assert_eq!(notice(&mut n)["title"], "mast unpaired this machine");
    d.shutdown();

    // on, but nobody answers
    let env = Env::new();
    let rec = Recorder::new();
    rec.set_unavailable(true);
    let d = Daemon::start(toasting(&env, "desk", &rec, 30_000)).unwrap();
    let mut c = Client::connect(d.socket_path());
    let mut n = watch(&d, "notice");
    c.result("trust", json!({"fingerprint": fpx(1), "name": "lap"}));
    assert!(d.send_ctl(CtlMsg::Revoked {
        peer: fpx(1).parse().unwrap()
    }));
    assert_eq!(notice(&mut n)["title"], "lap unpaired this machine");
    let counters = until("the toast counted", || {
        let st = c.result("status", json!(null));
        (st["counters"]["toasts_undelivered"] == 1).then(|| st["counters"].clone())
    });
    assert_eq!(counters["toasts_shown"], 1);
    d.shutdown();
}
