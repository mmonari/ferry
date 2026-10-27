//! `trust` swaps the live trust set and dials; refusals name the fingerprint. Loopback only.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::time::{Duration, Instant};

use ferry_net::{
    CloseCode, CloseReason, Endpoint, EndpointConfig, Event, Fingerprint, Identity, TrustSet,
};
use serde_json::{Value, json};
use support::*;

fn peer<'a>(status: &'a Value, fp: &str) -> Option<&'a Value> {
    status["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["fingerprint"] == fp)
}

#[test]
fn trust_makes_the_next_handshake_succeed_and_a_refusal_names_the_fingerprint() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());

    // An outside endpoint that knows the daemon but is not known to it.
    let me = Identity::ephemeral().unwrap();
    let my_fp = me.fingerprint();
    let trust: TrustSet = [d.fingerprint()].into_iter().collect();
    let rt = rt();
    let cfg = EndpointConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        ..Default::default()
    };
    let ep = {
        let _guard = rt.enter();
        Endpoint::bind(me, trust, cfg).unwrap()
    };

    // 1. not pinned by the daemon: refused
    let refused = rt.block_on(ep.dial(d.net_addr(), d.fingerprint()));
    assert!(refused.is_err(), "an unpinned peer must be refused");

    // ... and the daemon says so, naming the fingerprint
    let st = wait_for("the refusal in status", || {
        let st = c.result("status", json!(null));
        peer(&st, &my_fp.to_string()).is_some().then_some(st)
    });
    let p = peer(&st, &my_fp.to_string()).unwrap();
    assert_eq!(p["state"], "refused");
    assert!(
        p["detail"].as_str().unwrap().contains(&my_fp.to_string()),
        "{p}"
    );
    assert_eq!(st["counters"]["inbound_refused"], 1);

    // 2. `trust` without an address pins and accepts; the swap is live when it answers
    c.result(
        "trust",
        json!({"fingerprint": my_fp.to_string(), "name": "outsider"}),
    );
    let nc = rt
        .block_on(ep.dial(d.net_addr(), d.fingerprint()))
        .expect("pinned now");
    assert_eq!(nc.conn.peer_fp(), d.fingerprint());
    // A peer is up once both Hellos are in: the dialer speaks first.
    let mut hello = Vec::new();
    ferryd::fake_peer::hello_now("outsider", vec![]).encode(&mut hello);
    nc.conn.try_send_frame(hello.into()).unwrap();

    // the daemon reports the link, and the refusal is gone
    let st = wait_for("the link", || {
        let st = c.result("status", json!(null));
        (peer(&st, &my_fp.to_string())?["state"] == "connected").then_some(st)
    });
    let p = peer(&st, &my_fp.to_string()).unwrap();
    assert_eq!(p["name"], "outsider");
    assert!(p["addr"].as_str().unwrap().starts_with("127.0.0.1:"));
    assert!(p["rtt_us"].is_u64());
    assert_eq!(st["peers"].as_array().unwrap().len(), 1);

    // the peer goes away: the daemon says offline
    nc.conn.close(ferry_net::CloseCode::SHUTDOWN, "bye");
    wait_for("the link to drop", || {
        let st = c.result("status", json!(null));
        (peer(&st, &my_fp.to_string())?["state"] == "offline").then_some(())
    });
    d.shutdown();
}

#[test]
fn two_daemons_connect_once_both_pins_are_in() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (start(&ea), start(&eb));
    let (mut a, mut b) = (
        Client::connect(da.socket_path()),
        Client::connect(db.socket_path()),
    );
    let (fa, fb) = (da.fingerprint().to_string(), db.fingerprint().to_string());

    // B pins A and dials it, but A has not pinned B: refused on both sides
    b.result(
        "trust",
        json!({"fingerprint": fa, "name": "a", "addr": da.net_addr().to_string()}),
    );
    let st = wait_for("A refusing B", || {
        let st = a.result("status", json!(null));
        peer(&st, &fb).is_some().then_some(st)
    });
    assert_eq!(peer(&st, &fb).unwrap()["state"], "refused");
    assert!(
        peer(&st, &fb).unwrap()["detail"]
            .as_str()
            .unwrap()
            .contains(&fb)
    );
    let st = wait_for("B seeing the refusal", || {
        let st = b.result("status", json!(null));
        (peer(&st, &fa)?["state"] == "refused").then_some(st)
    });
    // B names the peer that refused it, in full
    assert_eq!(
        peer(&st, &fa).unwrap()["detail"],
        format!("peer {fa} does not trust this machine")
    );

    // A pins B (no address: it only accepts); B's retry connects
    a.result("trust", json!({"fingerprint": fb, "name": "b"}));
    wait_for("A connected to B", || {
        let st = a.result("status", json!(null));
        (peer(&st, &fb)?["state"] == "connected").then_some(())
    });
    wait_for("B connected to A", || {
        let st = b.result("status", json!(null));
        (peer(&st, &fa)?["state"] == "connected").then_some(())
    });
    da.shutdown();
    // B notices A leaving
    wait_for("B seeing A go", || {
        let st = b.result("status", json!(null));
        (peer(&st, &fa)?["state"] != "connected").then_some(())
    });
    db.shutdown();
}

#[test]
fn a_changed_fingerprint_is_named() {
    // An endpoint with identity X answers where the daemon expects Y.
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let impostor = Identity::ephemeral().unwrap();
    let impostor_fp = impostor.fingerprint();
    let expected = Fingerprint([0x55; 32]);
    let rt = rt();
    let ep = {
        let _guard = rt.enter();
        let trust: TrustSet = [d.fingerprint(), expected].into_iter().collect();
        let cfg = EndpointConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        };
        Endpoint::bind(impostor, trust, cfg).unwrap()
    };
    let addr = ep.local_addr().unwrap();
    let acceptor = ep.clone();
    rt.spawn(async move { while acceptor.accept().await.is_some() {} });

    // the daemon pins `expected` and dials the impostor's address
    c.result(
        "trust",
        json!({"fingerprint": expected.to_string(), "name": "x", "addr": addr.to_string()}),
    );
    let detail = wait_for("the fingerprint change", || {
        let st = c.result("status", json!(null));
        let p = peer(&st, &expected.to_string())?;
        (p["state"] == "refused").then(|| p["detail"].as_str().unwrap().to_string())
    });
    assert!(detail.contains("fingerprint changed"), "{detail}");
    assert!(detail.contains(&impostor_fp.short()), "{detail}");
    assert!(detail.contains(&expected.short()), "{detail}");
    d.shutdown();
}

/// An outside endpoint the daemon has pinned, on the runtime `rt`.
fn pinned_outsider(c: &mut Client, d: &ferryd::Daemon, rt: &tokio::runtime::Runtime) -> Endpoint {
    let me = Identity::ephemeral().unwrap();
    c.result(
        "trust",
        json!({"fingerprint": me.fingerprint().to_string(), "name": "outsider"}),
    );
    let _guard = rt.enter();
    let trust: TrustSet = [d.fingerprint()].into_iter().collect();
    let cfg = EndpointConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        ..Default::default()
    };
    Endpoint::bind(me, trust, cfg).unwrap()
}

#[test]
fn the_acceptor_closes_a_dialer_that_never_sends_hello() {
    // Either side closes with code 2 if no Hello arrives within 2 s.
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let rt = rt();
    let ep = pinned_outsider(&mut c, &d, &rt);

    // the outsider dials and then says nothing
    let mut nc = rt
        .block_on(ep.dial(d.net_addr(), d.fingerprint()))
        .expect("pinned");
    let opened = Instant::now();
    let closed = rt.block_on(async {
        // nothing but the close may arrive: the acceptor speaks only after the dialer's Hello
        tokio::time::timeout(Duration::from_secs(5), async {
            match nc.events.next().await {
                Some(Event::Closed(reason)) => reason,
                other => panic!("expected the close, got {other:?}"),
            }
        })
        .await
        .expect("the daemon never closed the silent dialer")
    });
    let after = opened.elapsed();
    assert_eq!(
        closed,
        CloseReason::Peer {
            code: CloseCode::PROTOCOL,
            reason: "no Hello".into()
        }
    );
    assert!(
        (Duration::from_millis(1900)..Duration::from_millis(3500)).contains(&after),
        "closed {after:?} after the connection opened"
    );
    d.shutdown();
}

#[test]
fn a_handshake_its_dialer_abandons_is_not_counted_as_a_refusal() {
    // A dial dropped while it opens, as a dial timeout drops it: the daemon receives the
    // dialer's close as a new connection that is closed on arrival (the netbook restart race
    // of 2026-10-16, design §3).
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let rt = rt();
    let ep = pinned_outsider(&mut c, &d, &rt);

    // polled once, so the connection exists, then dropped
    let dropped = rt.block_on(async {
        tokio::time::timeout(Duration::ZERO, ep.dial(d.net_addr(), d.fingerprint())).await
    });
    assert!(dropped.is_err(), "the dial finished in one poll");

    let st = wait_for("the abandoned handshake in the counters", || {
        let st = c.result("status", json!(null));
        (st["counters"]["inbound_abandoned"] == 1).then_some(st)
    });
    assert!(st["counters"]["inbound_refused"].is_null(), "{st}");
    d.shutdown();
}
