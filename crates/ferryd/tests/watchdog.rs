//! The daemon watchdog: `WATCHDOG=1` goes out while the `capture` and
//! `net` threads wake, and stops when either one stalls. A fake `NOTIFY_SOCKET`, a short period
//! (`ctl` ticks every quarter of the interval and the other two threads wake every half of it; here
//! 100 ms and 200 ms instead of 500 ms and 1 s), and the daemon's `stall` test hook in place of a
//! hang.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

use ferryd::watchdog::Thread;
use support::*;

/// systemd's `WatchdogSec` in these tests; `ctl` sends every quarter of it, the threads wake
/// every half of it (`PERIOD`).
const INTERVAL: Duration = Duration::from_millis(400);
const PERIOD: Duration = Duration::from_millis(200);

struct Fake {
    env: Env,
    sock: UnixDatagram,
    daemon: Option<ferryd::Daemon>,
}

impl Fake {
    fn start() -> Self {
        let env = Env::new();
        let path = env.tmp.path().join("notify.sock");
        let sock = UnixDatagram::bind(&path).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut o = options(&env);
        o.notify_socket = Some(path);
        o.watchdog = Some(INTERVAL);
        let daemon = ferryd::Daemon::start(o).unwrap();
        let me = Self {
            env,
            sock,
            daemon: Some(daemon),
        };
        assert_eq!(me.recv().1, "READY=1");
        me
    }

    fn recv(&self) -> (Instant, String) {
        let mut buf = [0u8; 128];
        let n = self.sock.recv(&mut buf).expect("no notification");
        (
            Instant::now(),
            std::str::from_utf8(&buf[..n]).unwrap().trim().to_string(),
        )
    }

    /// Every notification for the next `span`, with the time it came.
    fn collect(&self, span: Duration) -> Vec<(Instant, String)> {
        let end = Instant::now() + span;
        let mut out = Vec::new();
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return out;
            }
            self.sock.set_read_timeout(Some(left)).unwrap();
            let mut buf = [0u8; 128];
            match self.sock.recv(&mut buf) {
                Ok(n) => out.push((
                    Instant::now(),
                    std::str::from_utf8(&buf[..n]).unwrap().trim().to_string(),
                )),
                Err(_) => return out,
            }
        }
    }

    fn daemon(&self) -> &ferryd::Daemon {
        self.daemon.as_ref().unwrap()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.shutdown();
        }
    }
}

fn dogs(v: &[(Instant, String)]) -> Vec<Instant> {
    v.iter()
        .filter(|(_, m)| m == "WATCHDOG=1")
        .map(|(t, _)| *t)
        .collect()
}

#[test]
fn a_healthy_daemon_sends_a_heartbeat_every_period() {
    let f = Fake::start();
    let got = dogs(&f.collect(Duration::from_millis(2200)));
    // 2.2 s at one per 100 ms: about 22; allow a loaded host to miss a few, never a long gap.
    assert!(got.len() >= 12, "only {} heartbeats", got.len());
    let worst = got.windows(2).map(|w| w[1] - w[0]).max().unwrap();
    assert!(
        worst < INTERVAL,
        "a gap of {worst:?} would have been a watchdog kill"
    );
}

/// The §6.5 row: a stall in either thread stops `WATCHDOG=1` within 1.2 periods plus the tick (the
/// 1.2 s of the design, scaled), and it resumes when the thread wakes again.
fn watchdog_stops(thread: Thread) {
    let f = Fake::start();
    // Settle: heartbeats are flowing.
    assert!(dogs(&f.collect(Duration::from_millis(800))).len() >= 3);

    let stall = Duration::from_millis(2000);
    let t0 = Instant::now();
    f.daemon().stall(thread, stall);
    let got = dogs(&f.collect(Duration::from_millis(3200)));
    let during: Vec<Duration> = got.iter().map(|t| *t - t0).filter(|d| *d < stall).collect();
    // The thread beats once more at its next wake (up to one period away), then 1.2 periods
    // later it counts as stale, and ctl notices at its next tick (100 ms): silent after 4 periods.
    let last = during.last().copied().unwrap_or_default();
    assert!(
        last <= PERIOD * 4,
        "{thread:?}: WATCHDOG=1 still sent {last:?} after the stall began (heartbeats at {during:?})"
    );
    // ... and nothing between that and the end of the stall.
    assert!(
        !got.iter().any(|t| {
            let d = *t - t0;
            d > PERIOD * 4 && d < stall - PERIOD
        }),
        "{thread:?}: heartbeats during the stall: {during:?}"
    );
    // It comes back when the thread does.
    assert!(
        got.iter().any(|t| *t - t0 > stall),
        "{thread:?}: no heartbeat after the stall"
    );
}

#[test]
fn watchdog_stops_when_a_thread_stalls() {
    watchdog_stops(Thread::Capture);
    watchdog_stops(Thread::Net);
}

#[test]
fn the_shutdown_order_announces_stopping() {
    let mut f = Fake::start();
    let d = f.daemon.take().unwrap();
    let handle = std::thread::spawn(move || d.shutdown());
    let got = f.collect(Duration::from_millis(1500));
    handle.join().unwrap();
    assert!(
        got.iter().any(|(_, m)| m == "STOPPING=1"),
        "no STOPPING=1: {got:?}"
    );
    let _ = &f.env;
}

#[test]
fn no_heartbeat_without_a_watchdog() {
    let env = Env::new();
    let path = env.tmp.path().join("notify.sock");
    let sock = UnixDatagram::bind(&path).unwrap();
    let mut o = options(&env);
    o.notify_socket = Some(path);
    let d = ferryd::Daemon::start(o).unwrap();
    let mut buf = [0u8; 64];
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let n = sock.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"READY=1\n");
    sock.set_read_timeout(Some(Duration::from_millis(1500)))
        .unwrap();
    assert!(
        sock.recv(&mut buf).is_err(),
        "WATCHDOG=1 without WATCHDOG_USEC"
    );
    d.shutdown();
}
