//! A host name is resolved off the net thread's loop (exit stream). `getaddrinfo` can wait
//! seconds for DNS or mDNS (nss-mdns about 5 s); the loop beats the daemon watchdog
//! (`WatchdogSec=2s`), so a lookup inside it would stall the beat, systemd would kill ferryd, and
//! the restarted ferryd would redial the same name: a crash loop. Here the lookup is held by the
//! test, and the watchdog's fake `NOTIFY_SOCKET` shows that the heartbeat does not notice.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::net::SocketAddr;
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferryd::netaddr::Lookup;
use serde_json::{Value, json};
use support::*;
use tokio::sync::Semaphore;

/// systemd's `WatchdogSec` in these tests; the threads beat every half of it.
const INTERVAL: Duration = Duration::from_millis(400);

/// A lookup the test holds: each call waits for a permit, then answers with `answer`.
#[derive(Clone)]
struct Held {
    gate: Arc<Semaphore>,
    calls: Arc<Mutex<Vec<(String, u16)>>>,
    answer: Arc<Mutex<Option<SocketAddr>>>,
    /// Lookups that have produced their answer (it is then on its way to the net loop).
    answered: Arc<AtomicUsize>,
}

impl Held {
    fn new() -> Self {
        Self {
            gate: Arc::new(Semaphore::new(0)),
            calls: Arc::default(),
            answer: Arc::default(),
            answered: Arc::default(),
        }
    }

    fn lookup(&self) -> Lookup {
        let me = self.clone();
        Arc::new(move |host, port| {
            let me = me.clone();
            Box::pin(async move {
                me.calls.lock().unwrap().push((host, port));
                me.gate.acquire().await.unwrap().forget();
                let out = match *me.answer.lock().unwrap() {
                    Some(a) => Ok(vec![a]),
                    None => Err(std::io::Error::other("no such host (test)")),
                };
                me.answered.fetch_add(1, Ordering::SeqCst);
                out
            })
        })
    }

    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn answered(&self) -> usize {
        self.answered.load(Ordering::SeqCst)
    }

    fn release(&self, answer: Option<SocketAddr>) {
        *self.answer.lock().unwrap() = answer;
        self.gate.add_permits(1);
    }
}

/// A daemon that reports to a fake `NOTIFY_SOCKET` and resolves through `held`.
struct Watched {
    _env: Env,
    daemon: Option<ferryd::Daemon>,
    sock: UnixDatagram,
    c: Client,
    held: Held,
}

impl Watched {
    fn start(held: &Held) -> Self {
        let env = Env::new();
        let path = env.tmp.path().join("notify.sock");
        let sock = UnixDatagram::bind(&path).unwrap();
        let mut o = options(&env);
        o.notify_socket = Some(path);
        o.watchdog = Some(INTERVAL);
        o.lookup = Some(held.lookup());
        let daemon = ferryd::Daemon::start(o).unwrap();
        let c = Client::connect(daemon.socket_path());
        Self {
            _env: env,
            daemon: Some(daemon),
            sock,
            c,
            held: held.clone(),
        }
    }

    fn daemon(&self) -> &ferryd::Daemon {
        self.daemon.as_ref().unwrap()
    }

    /// The times of every `WATCHDOG=1` for the next `span`.
    fn heartbeats(&self, span: Duration) -> Vec<Instant> {
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
                Ok(n) if buf[..n].starts_with(b"WATCHDOG=1") => out.push(Instant::now()),
                Ok(_) => {}
                Err(_) => return out,
            }
        }
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        // a failed assertion must not hang in the shutdown behind a lookup nobody releases
        self.held.gate.add_permits(100);
        if let Some(d) = self.daemon.take() {
            d.shutdown();
        }
    }
}

fn peer<'a>(status: &'a Value, fp: &str) -> Option<&'a Value> {
    status["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["fingerprint"] == fp)
}

#[test]
fn a_slow_name_lookup_does_not_stall_the_net_heartbeat_or_its_commands() {
    let held = Held::new();
    let mut b = Watched::start(&held);
    let ea = Env::new();
    let da = start(&ea);
    let mut a = Client::connect(da.socket_path());
    let (fa, fb) = (
        da.fingerprint().to_string(),
        b.daemon().fingerprint().to_string(),
    );

    // b pins a by name; the lookup is now pending, and stays so
    let name = format!("slow.test:{}", da.net_addr().port());
    b.c.result(
        "trust",
        json!({"fingerprint": fa, "name": "a", "addr": name}),
    );
    wait_for("the lookup to start", || (held.calls() == 1).then_some(()));
    assert_eq!(
        held.calls.lock().unwrap()[0],
        ("slow.test".to_string(), da.net_addr().port())
    );

    // Heartbeats keep coming for 1.6 s (four intervals) while the lookup is pending. With the
    // lookup inside the loop, the net thread stops beating and ctl stops sending WATCHDOG=1.
    let t0 = Instant::now();
    let during = b.heartbeats(Duration::from_millis(1600));
    assert_eq!(held.calls(), 1, "the lookup was repeated");
    assert!(during.len() >= 6, "only {} heartbeats", during.len());
    let worst = during.windows(2).map(|w| w[1] - w[0]).max().unwrap();
    assert!(
        worst < INTERVAL,
        "a gap of {worst:?} would have been a watchdog kill"
    );
    assert!(t0.elapsed() < Duration::from_secs(3));

    // Commands to the net thread are served meanwhile: `trust` of another peer waits for the net
    // thread's ack of the new trust set, and `status` reads what net reports.
    let t1 = Instant::now();
    b.c.result("trust", json!({"fingerprint": fpx(9), "name": "other"}));
    let st = b.c.result("status", json!(null));
    assert!(
        t1.elapsed() < Duration::from_millis(400),
        "commands took {:?} behind a pending lookup",
        t1.elapsed()
    );
    assert!(peer(&st, &fa).is_some_and(|p| p["state"] != "connected"));

    // The answer arrives: b dials a, and once a pins b the link comes up.
    a.result("trust", json!({"fingerprint": fb, "name": "b"}));
    held.release(Some(da.net_addr()));
    wait_for("b connecting to a", || {
        let st = b.c.result("status", json!(null));
        (peer(&st, &fa)?["state"] == "connected").then_some(())
    });
    drop(b);
    da.shutdown();
}

#[test]
fn a_name_that_does_not_resolve_is_reported_as_before() {
    let held = Held::new();
    let mut b = Watched::start(&held);
    let fa = fpx(5);
    b.c.result(
        "trust",
        json!({"fingerprint": fa, "name": "gone", "addr": "gone.test:4243"}),
    );
    wait_for("the lookup to start", || (held.calls() == 1).then_some(()));
    held.release(None);
    let st = wait_for("the failure in status", || {
        let st = b.c.result("status", json!(null));
        peer(&st, &fa)?["detail"]
            .as_str()
            .is_some_and(|d| d.starts_with("cannot resolve gone.test:4243: "))
            .then_some(st)
    });
    let p = peer(&st, &fa).unwrap();
    assert_eq!(p["state"], "offline");
    assert!(
        p["detail"]
            .as_str()
            .unwrap()
            .contains("no such host (test)"),
        "{p}"
    );
}

#[test]
fn a_newer_dial_supersedes_a_lookup_still_pending() {
    let held = Held::new();
    let mut b = Watched::start(&held);
    let ea = Env::new();
    let da = start(&ea);
    let mut a = Client::connect(da.socket_path());
    let (fa, fb) = (
        da.fingerprint().to_string(),
        b.daemon().fingerprint().to_string(),
    );
    a.result("trust", json!({"fingerprint": fb, "name": "b"}));

    // the name first, held; then the IP literal for the same peer
    b.c.result(
        "trust",
        json!({"fingerprint": fa, "name": "a", "addr": "old.test:4243"}),
    );
    wait_for("the lookup to start", || (held.calls() == 1).then_some(()));
    b.c.result(
        "trust",
        json!({"fingerprint": fa, "name": "a", "addr": da.net_addr().to_string()}),
    );
    wait_for("the literal dial", || {
        let st = b.c.result("status", json!(null));
        (peer(&st, &fa)?["state"] == "connected").then_some(())
    });

    // The old lookup fails late; its answer must not touch the live link.
    held.release(None);
    wait_for("the old lookup to answer", || {
        (held.answered() == 1).then_some(())
    });
    // A sentinel (G-26): the net loop takes lookup answers from one queue, in order. Once a lookup
    // started after the old one has failed in `status`, the old answer has been looked at.
    b.c.result(
        "trust",
        json!({"fingerprint": fpx(9), "name": "sentinel", "addr": "sentinel.test:4243"}),
    );
    wait_for("the sentinel lookup to start", || {
        (held.calls() == 2).then_some(())
    });
    held.release(None);
    wait_for("the sentinel's failure", || {
        let st = b.c.result("status", json!(null));
        peer(&st, &fpx(9))?["detail"]
            .as_str()?
            .starts_with("cannot resolve sentinel.test")
            .then_some(())
    });
    let st = b.c.result("status", json!(null));
    let p = peer(&st, &fa).unwrap();
    assert_eq!(p["state"], "connected", "{p}");
    assert!(
        !p["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot resolve"),
        "{p}"
    );
    drop(b);
    da.shutdown();
}
