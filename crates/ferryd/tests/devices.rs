//! The production inject path (ferryd's `DeviceInjector` and ferry-inject's device owner) with
//! [`FakeDevices`] in place of the compositor, and a fake source on loopback. Nothing here
//! captures or injects.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferry_core::wire::{Control, Datagram, LeaveReason};
use ferry_net::Identity;
use ferryd::fake_peer::{FakePeer, Incoming, hello_now, monitor};
use ferryd::{Daemon, DeviceCall, FakeDevices, InjectBackend};
use serde_json::{Value, json};
use support::*;

const KEYMAP: &str = "xkb_keymap { fake };";
const SECOND: Duration = Duration::from_secs(1);
/// systemd's `WatchdogSec` here: ctl sends `WATCHDOG=1` every quarter of it while the threads
/// beat, and the threads wake every half of it.
const INTERVAL: Duration = Duration::from_millis(400);

fn key(code: u16, pressed: bool) -> DeviceCall {
    DeviceCall::Cmd(format!("Key {{ code: {code}, pressed: {pressed} }}"))
}

/// A target daemon whose devices are `fake`, with a watchdog on a fake notify socket, and the
/// fake source connected.
struct Rig {
    env: Env,
    d: Option<Daemon>,
    c: Client,
    rt: tokio::runtime::Runtime,
    p: FakePeer,
    fake: FakeDevices,
    /// When each `WATCHDOG=1` came, read as it came.
    beats: Arc<Mutex<Vec<Instant>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Rig {
    fn new() -> Self {
        let env = Env::new();
        let fake = FakeDevices::new();
        let path = env.tmp.path().join("notify.sock");
        let notify = UnixDatagram::bind(&path).unwrap();
        let mut o = options(&env);
        o.inject = InjectBackend::Devices(Box::new(fake.clone()));
        o.notify_socket = Some(path);
        o.watchdog = Some(INTERVAL);
        let d = Daemon::start(o).unwrap();
        let beats = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (beats, stop) = (Arc::clone(&beats), Arc::clone(&stop));
            notify
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            std::thread::spawn(move || {
                let mut buf = [0u8; 128];
                while !stop.load(Ordering::Relaxed) {
                    if let Ok(n) = notify.recv(&mut buf)
                        && buf[..n].starts_with(b"WATCHDOG=1")
                    {
                        beats.lock().unwrap().push(Instant::now());
                    }
                }
            });
        }
        // The start-up reset ran before `start` returned: devices made, synced, dropped.
        assert_eq!(
            fake.calls(),
            [DeviceCall::Open(None), DeviceCall::Sync, DeviceCall::Drop]
        );
        fake.clear();
        let mut c = Client::connect(d.socket_path());
        let id = Identity::ephemeral().unwrap();
        let fp = id.fingerprint().to_string();
        c.result("trust", json!({"fingerprint": fp, "name": "fake"}));
        let rt = rt();
        let mut p = rt
            .block_on(FakePeer::dial(id, d.net_addr(), d.fingerprint()))
            .unwrap();
        rt.block_on(p.exchange_hello(&hello_now("fake", vec![monitor("Virtual-1", 1280, 800)])))
            .expect("the daemon answers Hello");
        wait_for("the peer up", || {
            let st = c.result("status", json!(null));
            (st["peers"][0]["state"] == "connected").then_some(())
        });
        Rig {
            env,
            d: Some(d),
            c,
            rt,
            p,
            fake,
            beats,
            stop,
        }
    }

    /// The next control frame, within `within`.
    fn frame_within(&mut self, within: Duration) -> Control {
        self.rt
            .block_on(self.p.next_frame(within))
            .unwrap_or_else(|| panic!("no frame within {within:?}"))
    }

    /// Keymap and Enter; returns how long `HeldAck#1` took.
    fn enter(&mut self, x: f32, y: f32) -> Duration {
        self.p.keymap(KEYMAP).unwrap();
        let t = Instant::now();
        self.p.enter(x, y).unwrap();
        let ack = self.frame_within(3 * SECOND);
        assert_eq!(
            ack,
            Control::HeldAck {
                session: self.p.session,
                n: 1
            }
        );
        t.elapsed()
    }

    /// Ping, and how long the Pong took.
    fn ping(&mut self, id: u32) -> Duration {
        let t = Instant::now();
        self.p.send_datagram(Datagram::Ping { id, t_us: 0 });
        loop {
            match self.rt.block_on(self.p.next(3 * SECOND)) {
                Some(Incoming::Datagram(Datagram::Pong { id: got, .. })) if got == id => {
                    return t.elapsed();
                }
                Some(_) => {}
                None => panic!("no Pong {id}"),
            }
        }
    }

    fn status(&mut self) -> Value {
        self.c.result("status", json!(null))
    }

    /// The longest time between two `WATCHDOG=1`s from `from` to `to`, counting the edges.
    fn worst_heartbeat_gap(&self, from: Instant, to: Instant) -> Duration {
        let mut at: Vec<Instant> = self
            .beats
            .lock()
            .unwrap()
            .iter()
            .copied()
            .filter(|t| *t > from && *t < to)
            .collect();
        at.insert(0, from);
        at.push(to);
        at.windows(2).map(|w| w[1] - w[0]).max().unwrap_or_default()
    }
}

/// B-04, the acceptance test: creating the devices takes 2 s (a Wayland connect, roundtrips and
/// a Hyprland request that hang). The net thread used to do it inside `target_event`, on the
/// session's first input: `HeldAck#1` and the watchdog's `Ended` came 2 s late (scratch run on
/// a8fc13b: the first frame after `Enter` came after 2.00 s, an `Ended(watchdog)` due at
/// 650 ms). Now net acks every `Held`, answers `Ping`, keeps the daemon watchdog fed, and its
/// 650 ms watchdog ends the session on time, all while the devices are still being made. The
/// key pressed in that session never reaches the devices: the release dropped it first.
#[test]
fn a_slow_device_open_leaves_net_free() {
    let mut r = Rig::new();
    r.fake.set_open_delay(2 * SECOND);
    let t0 = Instant::now();
    let ack1 = r.enter(523.0, 0.0);
    assert!(
        ack1 < Duration::from_millis(300),
        "HeldAck#1 after {ack1:?}"
    );

    // Shift down and the Held cadence: every Held is acked at once.
    r.p.key(42, true).unwrap();
    let mut last_held = Instant::now();
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(100));
        last_held = Instant::now();
        r.p.held(&[42], &[]).unwrap();
        let (s, n) = (r.p.session, r.p.held_n);
        assert_eq!(r.frame_within(SECOND), Control::HeldAck { session: s, n });
        let took = last_held.elapsed();
        assert!(took < Duration::from_millis(300), "HeldAck after {took:?}");
    }
    let pong = r.ping(9);
    assert!(pong < Duration::from_millis(300), "Pong after {pong:?}");

    // The source goes silent: Ended(watchdog) 650 ms after the last Held.
    let ended = r.frame_within(2 * SECOND);
    let after = last_held.elapsed();
    assert_eq!(
        ended,
        Control::Ended {
            session: r.p.session,
            reason: LeaveReason::Watchdog.code()
        }
    );
    assert!(
        after >= Duration::from_millis(600) && after <= Duration::from_millis(650 + 100),
        "Ended(watchdog) {after:?} after the last Held"
    );
    // All of that happened while the devices were being made.
    assert!(
        t0.elapsed() < 2 * SECOND && r.fake.calls().is_empty(),
        "the devices came first: {:?} after Enter",
        t0.elapsed()
    );
    // The daemon watchdog was fed throughout: no gap a systemd would have killed for.
    let gap = r.worst_heartbeat_gap(t0, Instant::now());
    assert!(gap < INTERVAL, "WATCHDOG=1 stopped for {gap:?}");

    // Then the devices come, with the session's keymap, and get only the release.
    wait_for("the devices", || {
        r.fake
            .commands()
            .contains(&"ReleaseAll".to_string())
            .then_some(())
    });
    assert_eq!(
        r.fake.calls(),
        [
            DeviceCall::Open(Some(KEYMAP.into())),
            DeviceCall::Cmd("ReleaseAll".into())
        ]
    );
    let st = r.status();
    assert_eq!(st["last_release"]["reason"], "watchdog");
    r.d.take().unwrap().shutdown();
    let _ = &r.env;
}

/// The first input of a session waits for the devices, in order, and reaches them right after
/// their start-up reset (RG-SAFE-02: no key is dropped for being early). The source's `Held`s
/// are acked meanwhile. Keys and buttons only: a motion datagram travels on its own lane and may
/// overtake them, and queued motion keeps only the newest position.
#[test]
fn the_first_input_after_enter_reaches_the_devices_in_order() {
    let mut r = Rig::new();
    r.fake.set_open_delay(Duration::from_millis(500));
    let t0 = Instant::now();
    let ack1 = r.enter(523.0, 0.0);
    assert!(
        ack1 < Duration::from_millis(300),
        "HeldAck#1 after {ack1:?}"
    );
    r.p.key(30, true).unwrap();
    r.p.key(30, false).unwrap();
    r.p.button(0x110, true).unwrap();
    r.p.button(0x110, false).unwrap();
    r.p.key(42, true).unwrap();
    r.p.held(&[42], &[]).unwrap();
    let (s, n) = (r.p.session, r.p.held_n);
    assert_eq!(r.frame_within(SECOND), Control::HeldAck { session: s, n });
    assert!(
        t0.elapsed() < Duration::from_millis(500),
        "{:?}",
        t0.elapsed()
    );

    wait_for("the input at the devices", || {
        (r.fake.commands().len() >= 6).then_some(())
    });
    let timed = r.fake.timed();
    assert_eq!(
        timed[..7]
            .iter()
            .map(|(_, c)| c.clone())
            .collect::<Vec<_>>(),
        [
            DeviceCall::Open(Some(KEYMAP.into())),
            DeviceCall::Cmd("Motion { x: 523.0, y: 0.0 }".into()),
            key(30, true),
            key(30, false),
            DeviceCall::Cmd("Button { button: 272, pressed: true }".into()),
            DeviceCall::Cmd("Button { button: 272, pressed: false }".into()),
            key(42, true),
        ]
    );
    assert!(timed[0].0 - t0 >= Duration::from_millis(500));
    r.d.take().unwrap().shutdown();
}

/// B-05 on the production path: the devices' thread dies in the middle of a session. The session
/// ends as forced, the dead devices are dropped, and the next session gets new ones.
#[test]
fn a_dead_device_thread_ends_the_session() {
    let mut r = Rig::new();
    r.enter(10.0, 10.0);
    r.p.key(42, true).unwrap();
    wait_for("Shift down", || {
        r.fake.calls().contains(&key(42, true)).then_some(())
    });
    r.fake.die();
    std::thread::sleep(Duration::from_millis(50));
    r.p.key(30, true).unwrap();
    let session = r.p.session;
    assert_eq!(
        r.frame_within(2 * SECOND),
        Control::Ended {
            session,
            reason: LeaveReason::Forced.code()
        }
    );
    let st = r.status();
    assert_eq!(st["last_release"]["reason"], "forced");
    assert_eq!(st["counters"]["inject_failures"], 1);
    wait_for("the dead devices dropped", || {
        r.fake.calls().contains(&DeviceCall::Drop).then_some(())
    });

    r.fake.clear();
    r.enter(20.0, 20.0);
    r.p.key(30, true).unwrap();
    wait_for("a key on new devices", || {
        r.fake.calls().contains(&key(30, true)).then_some(())
    });
    assert_eq!(r.fake.calls()[0], DeviceCall::Open(Some(KEYMAP.into())));
    r.d.take().unwrap().shutdown();
}

/// Devices that cannot be made end the session as forced, as a dead inject thread does, once
/// per try: the session's later frames do not ask again.
///
/// Not `Rig::enter`: it takes `HeldAck#1` for the first frame. Here the open fails on the owner
/// thread while net still reads the source's `Enter` and `Held#1`, and net's loop picks between
/// the two events at random (`select!` is not biased). When the failure wins, `Ended(forced)`
/// goes out first and `Held#1` meets a session that is over, which is not acknowledged. Both
/// orders are right; the test waits for the `Ended` and takes a `HeldAck` before it as it comes.
#[test]
fn devices_that_cannot_be_made_end_the_session() {
    let mut r = Rig::new();
    r.fake.fail_opens(true);
    r.p.keymap(KEYMAP).unwrap();
    r.p.enter(10.0, 10.0).unwrap();
    for _ in 0..5 {
        r.p.motion(20.0, 20.0);
    }
    r.p.key(30, true).unwrap();
    let session = r.p.session;
    let ended = loop {
        if let m @ Control::Ended { .. } = r.frame_within(2 * SECOND) {
            break m;
        }
    };
    assert_eq!(
        ended,
        Control::Ended {
            session,
            reason: LeaveReason::Forced.code()
        }
    );
    assert_eq!(r.status()["counters"]["inject_failures"], 1);
    assert_eq!(r.fake.calls(), [DeviceCall::Refused]);
    r.d.take().unwrap().shutdown();
}
