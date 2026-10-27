//! Idle wake-ups of the heartbeat alone: its budget is 4 per second, of NFR-RES-01's ≤ 10 for a
//! connected daemon with capture on. With `WatchdogSec=2s` the heartbeat costs `ctl`
//! two wake-ups a second (the `WATCHDOG=1` sends, every 500 ms) and `capture` and `net` one each
//! (they wake every second to beat; `net`'s 2 s Ping rides on the same wake-up): 4.00 with no peer.
//! A thread's wake-up is a voluntary context switch, which is what `pidstat -w -t` counts. The
//! ticks come from `ticker::Ticker`: tokio's own interval costs two switches a tick, which made
//! the same schedule 7.2 a second.
//!
//! This test is alone in its binary on purpose: the process's other threads would add to the
//! count.
#![expect(
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "integration test: diagnostics for a failing run, which the harness captures; an unwrap that fails is the test failing"
)]

mod support;

use std::os::unix::net::UnixDatagram;
use std::time::Duration;

use support::*;

/// Voluntary context switches of the daemon's own threads, by name.
fn switches() -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for t in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let comm = std::fs::read_to_string(t.path().join("comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if !["ctl", "net", "capture"].contains(&comm.as_str()) {
            continue;
        }
        let status = std::fs::read_to_string(t.path().join("status")).unwrap_or_default();
        let n = status
            .lines()
            .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        out.push((comm, n));
    }
    out.sort();
    out
}

#[test]
fn an_idle_daemon_with_the_shipped_watchdog_wakes_at_most_four_times_a_second() {
    let env = Env::new();
    let path = env.tmp.path().join("notify.sock");
    let _sock = UnixDatagram::bind(&path).unwrap();
    let mut o = options(&env);
    o.notify_socket = Some(path);
    o.watchdog = Some(Duration::from_secs(2)); // WatchdogSec=2s
    let d = ferryd::Daemon::start(o).unwrap();

    std::thread::sleep(Duration::from_secs(2)); // start-up work settles
    let before = switches();
    let span = Duration::from_secs(10);
    std::thread::sleep(span);
    let after = switches();
    d.shutdown();

    let mut total = 0.0;
    for ((name, a), (name_b, b)) in before.iter().zip(&after) {
        assert_eq!(name, name_b);
        let per_s = (b - a) as f64 / span.as_secs_f64();
        eprintln!("{name}: {per_s:.2} wake-ups/s");
        total += per_s;
    }
    assert_eq!(before.len(), 3, "threads: {before:?}");
    eprintln!("total: {total:.2} wake-ups/s");
    assert!(
        total <= 4.2,
        "{total:.2} wake-ups/s: {before:?} -> {after:?}"
    );
    assert!(
        total >= 3.0,
        "{total:.2}: the heartbeat is not running? {before:?} -> {after:?}"
    );
}
