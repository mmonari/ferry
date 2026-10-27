//! A trace-level log of a whole pairing holds no code, no nonce and
//! no commitment. One test in its own binary, because it installs the global subscriber.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use ferry_core::pair::{PairAction, PairEvent, PairFrame, PairMachine, Role, Start, commitment};
use ferry_core::wire::control::Pair;
use ferry_core::{Actions, Micros, PeerId};
use ferry_net::{Endpoint, EndpointConfig, Event, Identity, TrustSet};
use serde_json::Value;
use support::*;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every form a 32-byte secret could take in a log line.
fn forms(bytes: &[u8; 32]) -> Vec<String> {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let dec = format!("{bytes:?}"); // [12, 34, …]
    vec![hex.clone(), hex.to_uppercase(), hex[..16].to_string(), dec]
}

#[test]
fn code_never_logged() {
    let log = Captured::default();
    let writer = log.clone();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            // no timestamps: their six-digit fractions could equal the code by chance
            .without_time()
            .with_writer(move || writer.clone())
            .finish(),
    )
    .unwrap();

    let eb = Env::new();
    let db = start(&eb);
    let stream = UnixStream::connect(db.socket_path()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream;
    let mut read = || {
        let mut l = String::new();
        reader.read_line(&mut l).unwrap();
        serde_json::from_str::<Value>(&l).unwrap()
    };
    read(); // hello
    w.write_all(b"{\"id\":1,\"method\":\"pair.accept\"}\n")
        .unwrap();
    assert!(read().get("result").is_some());

    // The test is the dialer: it knows its nonce, sees B's, and computes the commitment.
    let rt = rt();
    let id = Identity::ephemeral().unwrap();
    let a_fp = PeerId(id.fingerprint().0);
    let b_fp = PeerId(db.fingerprint().0);
    let ep = {
        let _g = rt.enter();
        Endpoint::bind(
            id,
            TrustSet::new(),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                ..EndpointConfig::default()
            },
        )
        .unwrap()
    };
    let mut pc = rt.block_on(ep.dial_pairing(db.net_addr())).unwrap();
    let n_a = [0x5c; 32];
    let mut m = PairMachine::new();
    let mut out = Actions::new();
    m.handle(
        PairEvent::Start(Start {
            role: Role::Dialer,
            me: a_fp,
            peer: b_fp,
            nonce: n_a,
            until: Micros(u64::MAX / 2),
            hello: ferryd::fake_peer::hello_now("tester", vec![]),
        }),
        Micros(1),
        &mut out,
    );
    let (mut code, mut n_b) = (None, None);
    rt.block_on(async {
        loop {
            for a in out.drain().collect::<Vec<_>>() {
                match a {
                    PairAction::Send(f) => {
                        let mut b = Vec::new();
                        f.encode(&mut b);
                        pc.conn.try_send_frame(Bytes::from(b)).unwrap();
                    }
                    PairAction::ShowCode { code: c, .. } => code = Some(c),
                    _ => {}
                }
            }
            if code.is_some() {
                return;
            }
            match tokio::time::timeout(Duration::from_secs(5), pc.events.next()).await {
                Ok(Some(Event::Frame(b))) => {
                    let f = PairFrame::decode(&b).unwrap();
                    if let PairFrame::Pair(Pair::Nonce { nonce }) = &f {
                        n_b = Some(*nonce);
                    }
                    m.handle(PairEvent::Frame(f), Micros(1), &mut out);
                }
                other => panic!("{other:?}"),
            }
        }
    });
    let code = code.unwrap();
    let n_b = n_b.unwrap();
    // B showed the same code to its client, then the user says no
    let ev = loop {
        let v = read();
        if v["event"] == "pairing" && v["data"]["stage"] == "code" {
            break v;
        }
    };
    assert_eq!(ev["data"]["code"], code.digits());
    w.write_all(b"{\"id\":2,\"method\":\"pair.answer\",\"params\":{\"match\":false}}\n")
        .unwrap();
    loop {
        let v = read();
        if v["event"] == "pairing" && v["data"]["stage"] == "failed" {
            break;
        }
    }
    drop(pc);
    db.shutdown();

    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    // A release build compiles TRACE out (`release_max_level_debug`): the log then
    // runs at the highest level the build keeps, and still has to see the pairing.
    let traced = tracing::level_filters::STATIC_MAX_LEVEL >= tracing::Level::TRACE;
    assert!(
        text.contains("pairing: the code is shown") && (!traced || text.contains("TRACE")),
        "the log ran at the build's highest level and saw the pairing ({} bytes)",
        text.len()
    );
    let mut secrets = vec![code.digits(), code.to_string()];
    secrets.extend(forms(&n_a));
    secrets.extend(forms(&n_b));
    secrets.extend(forms(&commitment(&a_fp, &b_fp, &n_a)));
    for s in &secrets {
        assert!(!text.contains(s.as_str()), "the log holds {s:?}");
    }
}
