//! Log levels from the M1 exit check: a pairing request with no window open is not a
//! version mismatch, and the shutdown order is not a fault. One test in its own binary, because
//! it installs the global subscriber.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferry_core::target::InjectCmd;
use ferry_core::wire::Control;
use ferry_net::{ConnectError, Endpoint, EndpointConfig, Identity, TrustSet};
use ferryd::Recorder;
use ferryd::fake_peer::{FakePeer, hello_now};
use serde_json::json;
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

impl Captured {
    fn lines(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Lines at WARN or ERROR.
    fn faults(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|l| l.contains("WARN") || l.contains("ERROR"))
            .collect()
    }
}

#[test]
fn exit_check_texts_and_levels() {
    let log = Captured::default();
    let writer = log.clone();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish(),
    )
    .unwrap();

    // Row 3: a pairing dial while no window is open. The dialer says "not accepting pairing";
    // the acceptor says what it was, at INFO.
    let env = Env::new();
    let rec = Recorder::new();
    let d = start_recording(&env, &rec);
    let mut c = Client::connect(d.socket_path());
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
    assert_eq!(
        rt.block_on(ep.dial_pairing(d.net_addr())).unwrap_err(),
        ConnectError::NotAccepting
    );
    let line = wait_for("the acceptor's line", || {
        log.lines()
            .into_iter()
            .find(|l| l.contains("refused a pairing request"))
    });
    assert!(line.contains("INFO"), "{line}");
    assert!(
        !log.lines().iter().any(|l| l.contains("version mismatch")),
        "{:#?}",
        log.lines()
    );
    let st = c.result("status", json!(null));
    assert!(st["counters"]["inbound_refused"].is_null(), "{st}");

    // Row 16 (stop-nb): SIGTERM while a source drives this peer. The capture thread stops
    // first, so the net thread's later sends to it are the shutdown order.
    let id = Identity::ephemeral().unwrap();
    let fp = id.fingerprint().to_string();
    c.result("trust", json!({"fingerprint": fp, "name": "fake"}));
    let mut p = rt
        .block_on(FakePeer::dial(id, d.net_addr(), d.fingerprint()))
        .unwrap();
    rt.block_on(p.exchange_hello(&hello_now("fake", vec![])))
        .expect("the daemon answers Hello");
    p.keymap("xkb_keymap { fake };").unwrap();
    p.enter(10.0, 10.0).unwrap();
    let ack = rt.block_on(p.next_frame(Duration::from_secs(2)));
    assert!(matches!(ack, Some(Control::HeldAck { .. })), "{ack:?}");
    p.key(29, true).unwrap(); // Ctrl held
    wait_for("Ctrl down", || {
        rec.commands()
            .contains(&InjectCmd::Key {
                code: 29,
                pressed: true,
            })
            .then_some(())
    });
    d.shutdown();
    assert!(
        log.lines()
            .iter()
            .any(|l| l.contains("the capture thread has stopped for the shutdown")),
        "the case is exercised: {:#?}",
        log.lines()
    );
    assert_eq!(log.faults(), Vec::<String>::new());
}
