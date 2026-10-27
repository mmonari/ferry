//! A trusted peer that loops `Error` or `PeerState` frames must not flood the
//! journal. `info` is the default level, so the bound is on `info` and above: the review saw
//! 31,253 `warn` lines in 4 s from `Error` frames and 68,960 `info` lines from `PeerState`.
//! One test in its own binary, because it installs the global subscriber.
#![expect(
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "integration test: diagnostics for a failing run, which the harness captures; an unwrap that fails is the test failing"
)]

mod support;

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ferry_core::wire::Control;
use ferry_net::Identity;
use ferryd::fake_peer::{FakePeer, hello_now};
use serde_json::json;
use support::*;

const FRAMES: u64 = 10_000;

static LINES: AtomicUsize = AtomicUsize::new(0);

/// Counts the lines the subscriber writes.
#[derive(Clone)]
struct CountLines;

impl Write for CountLines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        LINES.fetch_add(
            buf.iter().filter(|b| **b == b'\n').count(),
            Ordering::Relaxed,
        );
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Sends `FRAMES` frames as fast as the connection takes them; returns how long that took.
fn flood(
    rt: &tokio::runtime::Runtime,
    p: &mut FakePeer,
    frame: impl Fn(u64) -> Control,
) -> Duration {
    let t0 = Instant::now();
    rt.block_on(async {
        for i in 0..FRAMES {
            let frame = frame(i);
            while p.send(&frame).is_err() {
                tokio::time::sleep(Duration::from_micros(200)).await;
            }
        }
    });
    t0.elapsed()
}

#[test]
fn a_flood_of_error_and_peer_state_frames_is_a_few_lines_a_second() {
    // `info` is the default level of the shipped unit
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(|| CountLines)
            .finish(),
    )
    .unwrap();

    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let id = Identity::ephemeral().unwrap();
    let fp = id.fingerprint().to_string();
    c.result("trust", json!({"fingerprint": fp, "name": "noisy"}));
    let rt = rt();
    let mut p = rt
        .block_on(FakePeer::dial(id, d.net_addr(), d.fingerprint()))
        .unwrap();
    rt.block_on(p.exchange_hello(&hello_now("noisy", desk_monitors())))
        .expect("Hello back");
    wait_for("the peer up", || {
        let st = c.result("status", json!(null));
        (st["peers"][0]["state"] == "connected").then_some(())
    });

    let counter = |c: &mut Client, name: &str| {
        c.result("status", json!(null))["counters"][name]
            .as_u64()
            .unwrap_or(0)
    };
    let mut measure = |what: &str, frame: &dyn Fn(u64) -> Control, counter_name: &str| {
        let before = LINES.load(Ordering::Relaxed);
        let took = flood(&rt, &mut p, frame);
        wait_for(what, || {
            (counter(&mut c, counter_name) == FRAMES).then_some(())
        });
        let lines = LINES.load(Ordering::Relaxed) - before;
        // one line a second per site, plus the first of each, plus a margin for the edges
        let allowed = 4 + 2 * took.as_secs() as usize;
        assert!(
            lines <= allowed,
            "{what}: {lines} lines at info and above in {took:?} (at most {allowed})"
        );
        eprintln!("{what}: {FRAMES} frames, {lines} lines in {took:?}");
    };
    measure(
        "Error frames",
        &|_| Control::Error {
            code: 99,
            message: "x".repeat(400),
        },
        "peer_errors",
    );
    // alternating, so every frame is a change
    measure(
        "PeerState frames",
        &|i| Control::PeerState { locked: i % 2 == 0 },
        "peer_state_frames",
    );
    d.shutdown();
}
