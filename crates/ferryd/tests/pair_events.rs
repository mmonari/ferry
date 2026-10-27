//! Pairing as a stream (m3 design M3-D8): the code goes to every `pairing`
//! subscriber, a connection that subscribes in the middle of a pairing gets it too, and any of
//! them may answer; the first answer wins.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::time::Duration;

use ferryd::Daemon;
use serde_json::{Value, json};
use support::*;

const LONG: Duration = Duration::from_secs(10);

/// The `pairing` events of `c` until one at `stage`; every event before it is returned too.
fn until_stage(c: &mut Client, stage: &str) -> Vec<Value> {
    let mut seen = Vec::new();
    loop {
        let ev = c
            .event_within("pairing", LONG)
            .unwrap_or_else(|| panic!("no {stage} event; saw {seen:?}"));
        let data = ev["data"].clone();
        let got = data["stage"].as_str().unwrap().to_string();
        seen.push(data);
        if got == stage {
            return seen;
        }
        assert!(
            !matches!(got.as_str(), "failed" | "closed"),
            "waiting for {stage}: {seen:?}"
        );
    }
}

fn stages(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|e| e["stage"].as_str().unwrap())
        .collect()
}

/// A call on a connection that is subscribed: events may come before the answer, and they stay
/// for the reads that follow (a `paired` can be written before the answer to the `pair.answer`
/// that completes the pairing).
fn call(c: &mut Client, method: &str, params: Value) -> Value {
    let response = c.call_holding(method, params);
    assert!(response.get("error").is_none(), "{method}: {response}");
    response["result"].clone()
}

/// The error code and message of a call that must fail, on a subscribed connection, and the
/// events that came before the answer.
fn refused(c: &mut Client, method: &str, params: Value) -> (String, String, Vec<Value>) {
    let (response, events) = c.call_collecting(method, params);
    let e = &response["error"];
    (
        e["code"].as_str().unwrap().to_string(),
        e["message"].as_str().unwrap().to_string(),
        events,
    )
}

struct Pair {
    da: Daemon,
    db: Daemon,
    /// A: the dialer's connection. It started the pairing and gets its events without subscribing.
    a: Client,
    /// B: the connection that opened the acceptor's window, and subscribed to `pairing` as well.
    owner: Client,
    /// What `owner` heard before the answer to `pair.accept`.
    owner_early: Vec<Value>,
    /// B: the panel. It started nothing; it subscribed.
    panel: Client,
    envs: (Env, Env),
}

/// B opens its window, a panel subscribes on B, A dials: both are at the code.
fn at_the_code() -> Pair {
    let (ea, eb) = (Env::new(), Env::new());
    let mut oa = options(&ea);
    oa.name = Some("desktop".into());
    let mut ob = options(&eb);
    ob.name = Some("laptop".into());
    let (da, db) = (Daemon::start(oa).unwrap(), Daemon::start(ob).unwrap());
    let mut a = Client::connect(da.socket_path());
    let mut owner = Client::connect(db.socket_path());
    let mut panel = Client::connect(db.socket_path());

    // the owner listens to pairing too: it must hear each event once
    owner.subscribe(&["pairing"]);
    assert!(
        owner.read_within(Duration::from_millis(150)).is_none(),
        "no pairing, no event"
    );
    let (answer, owner_early) = owner.call_collecting("pair.accept", json!(null));
    assert!(answer.get("error").is_none(), "{answer}");
    let owner_early: Vec<Value> = owner_early
        .iter()
        .filter(|e| e["event"] == "pairing")
        .map(|e| e["data"].clone())
        .collect();

    // the panel opens while the window is open: it is told at once
    panel.subscribe(&["pairing", "state"]);
    let first = panel.event_within("pairing", LONG).unwrap();
    assert_eq!(first["data"]["stage"], "listening");
    assert_eq!(first["data"]["role"], "acceptor");

    // the dialer's first event can come before the answer to pair.start
    let answer = a.call_holding("pair.start", json!({"addr": db.net_addr().to_string()}));
    assert!(answer.get("error").is_none(), "{answer}");
    Pair {
        da,
        db,
        a,
        owner,
        owner_early,
        panel,
        envs: (ea, eb),
    }
}

#[test]
fn the_code_reaches_the_owner_and_every_subscriber_once_and_any_of_them_may_answer() {
    let mut p = at_the_code();
    let on_a = until_stage(&mut p.a, "code");
    // the owner subscribed, so it hears the window open and then the code, each once
    let mut on_owner = std::mem::take(&mut p.owner_early);
    on_owner.extend(until_stage(&mut p.owner, "code"));
    assert_eq!(stages(&on_owner), ["listening", "code"]);
    let on_panel = until_stage(&mut p.panel, "code");
    let code = on_a.last().unwrap()["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 6);
    assert_eq!(on_owner.last().unwrap()["code"], code.as_str());
    assert_eq!(
        on_panel.last().unwrap()["code"],
        code.as_str(),
        "the same code on the panel"
    );
    assert_eq!(on_panel.last().unwrap()["role"], "acceptor");
    assert_eq!(on_panel.last().unwrap()["peer"]["name"], "desktop");

    // a connection that opens now is told the code too, without asking
    let mut late = Client::connect(p.db.socket_path());
    late.subscribe(&["pairing"]);
    let snapshot = late.event_within("pairing", LONG).unwrap();
    assert_eq!(snapshot["data"]["stage"], "code");
    assert_eq!(snapshot["data"]["code"], code.as_str());

    // the stage is in the `state` topic, which a message from the net thread changed
    let state = loop {
        let ev = p.panel.event_within("state", LONG).expect("no state event");
        if ev["data"]["pairing"]["stage"] == "code" {
            break ev;
        }
    };
    assert_eq!(state["data"]["pairing"]["role"], "acceptor");

    // never in the status, on any connection or in any state event
    assert!(
        !state.to_string().contains(&code),
        "the code is not in a state event"
    );
    let st = call(&mut p.panel, "status", json!(null));
    assert_eq!(st["pairing"]["stage"], "code");
    assert!(
        !st.to_string().contains(&code),
        "the code is not in the status"
    );

    // the panel answers: the first answer wins
    call(&mut p.panel, "pair.answer", json!({"match": true}));
    let (code_err, message, owner_early) =
        refused(&mut p.owner, "pair.answer", json!({"match": false}));
    assert_eq!(code_err, "no_pairing", "{message}");
    assert!(
        message.contains("already answered") || message.contains("no code to answer"),
        "{message}"
    );

    // so the pairing goes on to the other side, and ends paired on both
    call(&mut p.a, "pair.answer", json!({"match": true}));
    // (events that came before the owner's refusal count as heard)
    let mut owner_rest: Vec<Value> = owner_early
        .iter()
        .filter(|e| e["event"] == "pairing")
        .map(|e| e["data"].clone())
        .collect();
    owner_rest.extend(until_stage(&mut p.owner, "paired"));
    let panel_rest = until_stage(&mut p.panel, "paired");
    until_stage(&mut p.a, "paired");
    // each event once: the owner got it on the connection that started the pairing, and not a
    // second time as a subscriber
    assert_eq!(stages(&owner_rest), ["waiting_peer", "paired"]);
    assert_eq!(stages(&panel_rest), ["waiting_peer", "paired"]);
    // and the one who answered after the first got no second answer through
    let trusted = std::fs::read_to_string(trusted_file(&p)).unwrap();
    assert!(trusted.contains("desktop"), "{trusted}");
    p.da.shutdown();
    p.db.shutdown();
}

fn trusted_file(p: &Pair) -> std::path::PathBuf {
    p.envs.1.paths().trusted_file()
}

#[test]
fn a_no_from_a_subscriber_ends_the_attempt_for_everyone() {
    let mut p = at_the_code();
    until_stage(&mut p.a, "code");
    until_stage(&mut p.panel, "code");
    // the panel says the codes differ
    call(&mut p.panel, "pair.answer", json!({"match": false}));
    let failed = until_stage(&mut p.panel, "failed");
    assert_eq!(failed.last().unwrap()["reason"], "rejected");
    // the dialer is told too, and nothing is pinned on either side
    let on_a = until_stage(&mut p.a, "failed");
    assert!(on_a.last().unwrap()["reason"].is_string());
    assert!(!p.envs.1.paths().trusted_file().exists());
    p.da.shutdown();
    p.db.shutdown();
}

#[test]
fn a_pairing_that_is_over_leaves_nothing_to_tell_a_late_subscriber() {
    let mut p = at_the_code();
    until_stage(&mut p.a, "code");
    until_stage(&mut p.panel, "code");
    call(&mut p.panel, "pair.answer", json!({"match": true}));
    call(&mut p.a, "pair.answer", json!({"match": true}));
    until_stage(&mut p.panel, "paired");
    until_stage(&mut p.a, "paired");

    let mut late = Client::connect(p.db.socket_path());
    late.subscribe(&["pairing", "peers"]);
    // the first line is the peers snapshot: no pairing event precedes it
    let first = late.read_line();
    assert_eq!(first["event"], "peers", "{first}");
    assert_eq!(first["data"]["peers"][0]["name"], "desktop");
    // The link coming up changes `peers` again, without a request: the net thread's messages
    // reach the subscribers too. The snapshot can already show it up (the link is not tied to
    // the pairing's last event, and a loaded host subscribes late), then there is nothing to wait
    // for. A pairing that is over says nothing more.
    let mut connected = first["data"]["peers"][0]["state"] == "connected";
    while !connected {
        let ev = late
            .event_within("peers", LONG)
            .expect("the peer never showed as connected");
        connected = ev["data"]["peers"][0]["state"] == "connected";
    }
    assert!(
        late.lines_until_quiet(Duration::from_millis(250))
            .iter()
            .all(|e| e["event"] == "peers")
    );
    p.da.shutdown();
    p.db.shutdown();
}

#[test]
fn the_dialer_that_subscribed_before_it_asked_hears_each_event_once_too() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (start(&ea), start(&eb));
    let mut b = Client::connect(db.socket_path());
    b.result("pair.accept", json!(null));
    let mut a = Client::connect(da.socket_path());
    a.subscribe(&["pairing"]);
    let (answer, early) =
        a.call_collecting("pair.start", json!({"addr": db.net_addr().to_string()}));
    assert!(answer.get("error").is_none(), "{answer}");
    let mut seen: Vec<Value> = early.iter().map(|e| e["data"].clone()).collect();
    seen.extend(until_stage(&mut a, "code"));
    let mut deduped = stages(&seen);
    deduped.dedup();
    assert_eq!(stages(&seen), deduped, "an event twice: {seen:?}");
    assert_eq!(stages(&seen), ["connecting", "code"]);
    assert_eq!(seen.last().unwrap()["role"], "dialer");
    // nothing more is queued for it, from the hub or from the sink
    assert!(a.read_within(Duration::from_millis(250)).is_none());
    call(&mut a, "pair.cancel", json!(null));
    da.shutdown();
    db.shutdown();
}

/// The window opening is a stage: a `pairing` subscriber that was already there
/// hears it. The connection that opened the window, if it did not subscribe, gets M1's stream: no
/// event before or after the answer to `pair.accept` until a request arrives.
#[test]
fn a_subscriber_hears_the_window_open_and_an_opener_that_did_not_subscribe_does_not() {
    let env = Env::new();
    let d = start(&env);
    let mut panel = Client::connect(d.socket_path());
    panel.subscribe(&["pairing"]);
    assert!(panel.read_within(Duration::from_millis(150)).is_none());

    // `result` fails on any line that is not the answer: M1's clients rely on that
    let mut opener = Client::connect(d.socket_path());
    let r = opener.result("pair.accept", json!(null));
    assert_eq!(r["expires_ms"], 120_000);

    let ev = panel.event_within("pairing", LONG).unwrap();
    let data = &ev["data"];
    assert_eq!(data["stage"], "listening");
    assert_eq!(data["role"], "acceptor");
    assert!(
        data.get("peer").is_none() && data.get("code").is_none(),
        "{data}"
    );
    let left = data["expires_ms"].as_u64().unwrap();
    assert!((100_000..=120_000).contains(&left), "{left}");
    // the opener has its answer and nothing else
    assert!(opener.read_within(Duration::from_millis(250)).is_none());
    assert_eq!(
        opener.result("status", json!(null))["pairing"]["stage"],
        "listening"
    );

    // closing the window is a stage too, and the opener, still not subscribed, hears it on its own
    // sink as before
    let (answer, _) = opener.call_collecting("pair.cancel", json!(null));
    assert!(answer.get("error").is_none(), "{answer}");
    let closed = until_stage(&mut panel, "closed");
    assert_eq!(closed.last().unwrap()["reason"], "cancelled");
    d.shutdown();
}
