//! Both peers capture, and lock to this machine, in process: a daemon
//! whose capture side runs against a [`FakePortal`] and whose target input goes to a
//! [`Recorder`], and a fake peer on loopback. Nothing here captures or injects.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::time::Duration;

use ferry_core::source::{PortalCmd, SourceEvent};
use ferry_core::wire::{self, Control, Datagram, LeaveReason};
use ferry_net::Identity;
use ferryd::fake_peer::{FakePeer, Incoming, hello_now, monitor};
use ferryd::probe::PROBE_ID_BIT;
use ferryd::watchdog::Thread;
use ferryd::{Daemon, DeviceCall, FakeDevices, FakePortal, InjectBackend, Recorder};
use serde_json::{Value, json};
use support::*;

const SECOND: Duration = Duration::from_secs(1);

fn peer<'a>(status: &'a Value, fp: &str) -> Option<&'a Value> {
    status["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["fingerprint"] == fp)
}

struct Rig {
    env: Env,
    d: Option<Daemon>,
    c: Client,
    rt: tokio::runtime::Runtime,
    p: FakePeer,
    fp: String,
    rec: Recorder,
    portal: FakePortal,
}

impl Rig {
    /// A capturing daemon (fake portal) and a fake peer whose Hello lists `features`, placed
    /// below HDMI-A-1 so the daemon's barriers arm.
    fn new(features: &[&str]) -> Self {
        let rec = Recorder::new();
        Self::with_inject(features, InjectBackend::Custom(Box::new(rec.clone())), rec)
    }

    /// The same, with the production inject path over `fake` devices (`rec` stays empty).
    fn with_devices(features: &[&str], fake: &FakeDevices) -> Self {
        let r = Self::with_inject(
            features,
            InjectBackend::Devices(Box::new(fake.clone())),
            Recorder::new(),
        );
        fake.clear();
        r
    }

    fn with_inject(features: &[&str], inject: InjectBackend, rec: Recorder) -> Self {
        let env = Env::new();
        let portal = FakePortal::new();
        let mut o = options(&env);
        o.inject = inject;
        o.fake_portal = Some(portal.clone());
        let d = Daemon::start(o).unwrap();
        let mut c = Client::connect(d.socket_path());
        let id = Identity::ephemeral().unwrap();
        let fp = id.fingerprint().to_string();
        c.result("trust", json!({"fingerprint": fp, "name": "fake"}));
        let rt = rt();
        let mut p = rt
            .block_on(FakePeer::dial(id, d.net_addr(), d.fingerprint()))
            .unwrap();
        let mut hello = hello_now("fake", vec![monitor("eDP-1", 1366, 768)]);
        if let Control::Hello { features: f, .. } = &mut hello {
            *f = features.iter().map(|s| s.to_string()).collect();
        }
        rt.block_on(p.exchange_hello(&hello))
            .expect("the daemon answers Hello");
        wait_for("the peer up", || {
            let st = c.result("status", json!(null));
            (peer(&st, &fp)?["state"] == "connected").then_some(())
        });
        c.result(
            "layout.place",
            json!({"peer": "fake", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
        );
        let mut r = Rig {
            env,
            d: Some(d),
            c,
            rt,
            p,
            fp,
            rec,
            portal,
        };
        r.wait_gate("armed", None);
        r.portal_ran(0, &PortalCmd::Enable);
        r
    }

    fn status(&mut self) -> Value {
        self.c.result("status", json!(null))
    }

    /// Waits until `capture.gate` and `capture.off_reason` are these.
    fn wait_gate(&mut self, gate: &str, off: Option<&str>) -> Value {
        let c = &mut self.c;
        wait_for(&format!("gate {gate}, off_reason {off:?}"), || {
            let st = c.result("status", json!(null));
            let reason = st["capture"].get("off_reason").and_then(Value::as_str);
            (st["capture"]["gate"] == gate && reason == off).then_some(st)
        })
    }

    /// Waits until the fake portal has run `cmd` at index `from` of its log or later, and returns
    /// the log from `from` on. `status` shows a gate as soon as the capture thread decides it;
    /// the portal command that goes with it travels on through the control task, so right
    /// after [`Rig::wait_gate`] the log may still lack it.
    fn portal_ran(&self, from: usize, cmd: &PortalCmd) -> Vec<PortalCmd> {
        wait_for(&format!("{cmd:?} at the portal"), || {
            let log = self.portal.commands();
            let since = log.get(from..)?;
            since.contains(cmd).then(|| since.to_vec())
        })
    }

    /// The next control frame that `want` accepts, skipping the others (Layout, Keymap, …).
    fn frame_where(&mut self, what: &str, want: impl Fn(&Control) -> bool) -> Control {
        let until = std::time::Instant::now() + 3 * SECOND;
        loop {
            let left = until
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or_else(|| panic!("no {what} within 3 s"));
            match self.rt.block_on(self.p.next_frame(left)) {
                Some(m) if want(&m) => return m,
                Some(_) => {}
                None => panic!("no {what} within 3 s"),
            }
        }
    }

    fn shutdown(&mut self) {
        if let Some(d) = self.d.take() {
            d.shutdown();
        }
    }

    fn daemon(&self) -> &Daemon {
        self.d.as_ref().unwrap()
    }

    /// The daemon crosses into the fake: an EIS keymap, an activation, the `Enter`. Returns when
    /// the fake got the `Enter`.
    fn cross(&mut self) -> std::time::Instant {
        assert!(self.portal.event(SourceEvent::Keymap {
            text: "xkb_keymap { desk };".into()
        }));
        self.frame_where("the daemon's keymap", |m| {
            matches!(m, Control::Keymap { .. })
        });
        assert!(self.portal.event(SourceEvent::StartEmulating));
        assert!(self.portal.event(SourceEvent::Activated {
            activation_id: 10,
            barrier_id: 1,
            x: 800.0,
            y: 1081.0,
            lock: ferry_core::source::LockCheck::Unlocked,
        }));
        self.frame_where("the daemon's Enter", |m| matches!(m, Control::Enter { .. }));
        std::time::Instant::now()
    }

    /// When the fake portal ran its first `Release` at or after `since`, waiting up to 5 s.
    fn release_after(&self, since: std::time::Instant) -> std::time::Instant {
        wait_for("a portal Release", || {
            self.portal
                .timed()
                .into_iter()
                .find(|(t, c)| *t >= since && matches!(c, PortalCmd::Release { .. }))
                .map(|(t, _)| t)
        })
    }
}

/// How long after `since` the daemon answered a `Ping`, skipping frames.
fn pong_after(r: &mut Rig, id: u32, since: std::time::Instant) -> Duration {
    loop {
        match r.rt.block_on(r.p.next(3 * SECOND)) {
            Some(Incoming::Datagram(Datagram::Pong { id: got, .. })) if got == id => {
                return since.elapsed();
            }
            Some(_) => {}
            None => panic!("no Pong {id}"),
        }
    }
}

/// When `fake` last dropped its devices.
fn last_drop(fake: &FakeDevices) -> std::time::Instant {
    fake.timed()
        .iter()
        .rev()
        .find(|(_, c)| *c == DeviceCall::Drop)
        .map(|(t, _)| *t)
        .expect("a drop")
}

/// Two `Monitors` frames whose sets differ, so ctl takes each one as a change.
fn monitors_pair() -> [Control; 2] {
    [
        Control::Monitors {
            monitors: vec![monitor("eDP-1", 1366, 768)],
        },
        Control::Monitors {
            monitors: vec![monitor("eDP-1", 1280, 800)],
        },
    ]
}

/// A fake peer enters, then the local source tries. The gate is off from the
/// Enter until the session ends and the virtual devices are gone; then it re-arms. An Enter
/// that meets this peer's own busy source is refused.
#[test]
fn both_sides_capture() {
    let mut r = Rig::new(&[]);
    let armed_at = r.portal.commands().len();

    // The fake peer drives the daemon.
    r.p.keymap("xkb_keymap { fake };").unwrap();
    r.p.enter(523.0, 0.0).unwrap();
    r.frame_where("HeldAck#1", |m| matches!(m, Control::HeldAck { n: 1, .. }));
    let st = r.wait_gate("idle", Some("controlled"));
    assert_eq!(st["role"], "target");
    assert_eq!(
        r.portal_ran(armed_at, &PortalCmd::Disable),
        [PortalCmd::Disable],
        "capture goes off, nothing else"
    );

    // The local source tries while controlled: it bounces (a Release), nothing crosses.
    assert!(r.portal.event(SourceEvent::Activated {
        activation_id: 9,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: ferry_core::source::LockCheck::Unlocked,
    }));
    wait_for("the bounce", || {
        r.portal
            .commands()
            .iter()
            .any(|c| matches!(c, PortalCmd::Release { .. }))
            .then_some(())
    });
    assert_eq!(r.status()["role"], "target");

    // The session ends: release, devices synced and dropped, and only then the barriers.
    r.p.leave(LeaveReason::Home).unwrap();
    r.wait_gate("armed", None);
    r.portal_ran(armed_at, &PortalCmd::Enable);
    let devices = r.rec.device_calls();
    let (sync_at, drop_at) = match devices.as_slice() {
        // start-up: sync + drop after the modifier reset; then the session's end
        [_, _, (s, "sync"), (d, "drop")] => (*s, *d),
        other => panic!("{other:?}"),
    };
    let release_at = r
        .rec
        .timed()
        .iter()
        .rev()
        .find(|(_, c)| *c == ferry_core::target::InjectCmd::ReleaseAll)
        .map(|(t, _)| *t)
        .expect("a ReleaseAll at the end");
    assert!(release_at <= sync_at && sync_at <= drop_at);
    let rearm = r.portal.timed();
    let (set_at, _) = rearm
        .iter()
        .rev()
        .find(|(_, c)| matches!(c, PortalCmd::SetBarriers { .. }))
        .expect("barriers again");
    assert!(*set_at >= drop_at, "barriers before the devices were gone");
    assert_eq!(
        rearm.last().map(|(_, c)| c),
        Some(&PortalCmd::Enable),
        "{rearm:?}"
    );

    // Now the daemon is the source: a keymap from EIS, an activation, an Enter to the fake.
    assert!(r.portal.event(SourceEvent::Keymap {
        text: "xkb_keymap { desk };".into()
    }));
    r.frame_where("the daemon's keymap", |m| {
        matches!(m, Control::Keymap { .. })
    });
    assert!(r.portal.event(SourceEvent::StartEmulating));
    assert!(r.portal.event(SourceEvent::Activated {
        activation_id: 10,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: ferry_core::source::LockCheck::Unlocked,
    }));
    r.frame_where("the daemon's Enter", |m| matches!(m, Control::Enter { .. }));
    let c = &mut r.c;
    wait_for("role source", || {
        (c.result("status", json!(null))["role"] == "source").then_some(())
    });
    // The fake's own source pushes back at the same moment: refused.
    r.p.enter(10.0, 10.0).unwrap();
    let session = r.p.session;
    let ended = r.frame_where("Ended(refused)", |m| matches!(m, Control::Ended { .. }));
    assert_eq!(
        ended,
        Control::Ended {
            session,
            reason: LeaveReason::Refused.code()
        }
    );
    let st = r.status();
    assert_eq!(st["counters"]["refused_enters"], 1);
    r.shutdown();
}

/// Lock to this machine survives a restart, toggles, and travels
/// to a peer that lists `peer-state` as #14.
#[test]
fn lock_persists() {
    let mut r = Rig::new(&[wire::FEATURE_PEER_STATE]);
    // Peer up: this build tells a peer with the feature its state at once.
    r.frame_where("PeerState at peer up", |m| {
        *m == Control::PeerState { locked: false }
    });
    assert_eq!(
        r.c.result("lock", json!({"on": true})),
        json!({"locked": true})
    );
    r.frame_where("PeerState(locked)", |m| {
        *m == Control::PeerState { locked: true }
    });
    let st = r.wait_gate("idle", Some("locked_here"));
    assert_eq!(st["locked"], true);
    let file = r.env.paths().state_file();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "locked = true\n");

    // The peer locks itself: its links are walls here, and status says so.
    r.p.send(&Control::PeerState { locked: true }).unwrap();
    let fp = r.fp.clone();
    let c = &mut r.c;
    wait_for("peers[].locked", || {
        let st = c.result("status", json!(null));
        (peer(&st, &fp)?["locked"] == true).then_some(())
    });
    // `ferry peers` shows the same lock.
    let peers = c.result("peers", json!(null));
    assert_eq!(peer(&peers, &fp).unwrap()["locked"], true, "{peers}");

    // Restart on the same files: still locked, before any peer connects.
    r.shutdown();
    let d = start(&r.env);
    let mut c = Client::connect(d.socket_path());
    assert_eq!(c.result("status", json!(null))["locked"], true);
    // Toggle: `{}` flips it.
    assert_eq!(c.result("lock", json!({})), json!({"locked": false}));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "locked = false\n");
    assert_eq!(c.result("status", json!(null))["locked"], false);
    d.shutdown();
}

/// A peer without `peer-state` (an older build) never gets #14.
#[test]
fn peer_state_only_to_a_peer_with_the_feature() {
    let mut r = Rig::new(&[]);
    assert_eq!(
        r.c.result("lock", json!({"on": true})),
        json!({"locked": true})
    );
    r.wait_gate("idle", Some("locked_here"));
    // Unlock: the barriers come back, and still no PeerState went out.
    assert_eq!(
        r.c.result("lock", json!({"on": false})),
        json!({"locked": false})
    );
    r.wait_gate("armed", None);
    while let Some(m) = r.rt.block_on(r.p.next_frame(SECOND / 2)) {
        assert!(!matches!(m, Control::PeerState { .. }), "{m:?}");
    }
    r.shutdown();
}

/// Step 6's `status` fields from four streams, over the socket and at once (exit stream):
/// `locked`, `capture.off_reason`, `pairing`, `peers[].locked` and `peers[].last_seen`. (The
/// `capture.notice` needs Hyprland's IPC: `state::tests::status_carries_the_step_6_fields_together`.)
#[test]
fn status_carries_every_step_6_field_at_once() {
    let mut r = Rig::new(&[wire::FEATURE_PEER_STATE]);
    r.c.result("lock", json!({"on": true}));
    r.wait_gate("idle", Some("locked_here"));
    r.p.send(&Control::PeerState { locked: true }).unwrap();
    // the pairing belongs to the connection that opened it: this one
    r.c.result("pair.accept", json!({}));
    let fp = r.fp.clone();
    let c = &mut r.c;
    let st = wait_for("peers[].locked", || {
        let st = c.result("status", json!(null));
        (peer(&st, &fp)?["locked"] == true).then_some(st)
    });
    assert_eq!(st["locked"], true);
    assert_eq!(st["capture"]["off_reason"], "locked_here");
    assert_eq!(st["pairing"]["role"], "acceptor");
    assert_eq!(st["pairing"]["stage"], "listening");
    assert!(
        st["pairing"]["expires_ms"].as_u64().unwrap() > 100_000,
        "{st}"
    );
    let p = peer(&st, &fp).unwrap();
    assert!(
        p["last_seen"].as_str().is_some_and(|t| t.ends_with('Z')),
        "an up connection stamps last_seen: {p}"
    );
    // the code is never in status
    assert!(!st.to_string().contains("code"), "{st}");
    r.shutdown();
}

/// Exit run 1, D19 rep 16: `unpair` wrote `trusted.toml` (two `fsync`s) before it closed the peer's
/// connection, and under disk load the release came 181 ms after the request against a 100 ms bound.
/// The connection closes first now; if the write then fails, the pin goes back and the caller is told.
/// With the old order nothing closes: the write fails first and the peer never notices.
#[test]
fn unpair_closes_the_connection_before_it_writes_and_puts_the_pin_back_if_that_fails() {
    use std::os::unix::fs::PermissionsExt;

    let mut r = Rig::new(&[]);
    let trusted = r.env.paths().trusted_file();
    let dir = trusted.parent().unwrap().to_path_buf();
    let before = std::fs::read_to_string(&trusted).unwrap();
    // a directory that cannot take the temporary file `trusted.toml` is replaced through
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (code, ..) = r.c.error("unpair", json!({"peer": "fake"}));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(code, "internal");

    // the peer saw the close first: code 4, unpaired
    let closed = loop {
        match r.rt.block_on(r.p.next(3 * SECOND)) {
            Some(ferryd::fake_peer::Incoming::Closed(why)) => break why.to_string(),
            Some(_) => {}
            None => panic!("the connection was not closed before the failed write"),
        }
    };
    assert!(closed.contains("unpaired"), "{closed}");

    // and the pin is back, in memory and on disk
    assert_eq!(std::fs::read_to_string(&trusted).unwrap(), before);
    let st = r.status();
    assert!(peer(&st, &r.fp).is_some(), "the pin is gone: {st}");
    r.shutdown();
}

/// The daemon crosses into a fake target that never answers. It probes the quiet
/// target from 200 ms after the `Enter`, releases for silence at 650 ms, keeps probing the now
/// suspect peer every 100 ms, and one `Pong` clears suspect at once (before: only the dialer's
/// 2 s Ping cleared it, and the fake dialled, so this daemon never pinged at all).
#[test]
fn a_quiet_target_is_probed_and_one_pong_clears_suspect() {
    let mut r = Rig::new(&[]);
    assert!(r.portal.event(SourceEvent::Keymap {
        text: "xkb_keymap { desk };".into()
    }));
    r.frame_where("the daemon's keymap", |m| {
        matches!(m, Control::Keymap { .. })
    });
    assert!(r.portal.event(SourceEvent::StartEmulating));
    assert!(r.portal.event(SourceEvent::Activated {
        activation_id: 10,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: ferry_core::source::LockCheck::Unlocked,
    }));
    r.frame_where("the daemon's Enter", |m| matches!(m, Control::Enter { .. }));
    let entered = std::time::Instant::now();

    // The fake stays silent for 1 s and records what comes.
    let mut probes = Vec::new();
    let mut leave = None;
    let until = entered + SECOND;
    while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
        match r.rt.block_on(r.p.next(left)) {
            Some(Incoming::Datagram(Datagram::Ping { id, .. })) => {
                assert!(id & PROBE_ID_BIT != 0, "a stall probe id: {id:#x}");
                probes.push((entered.elapsed(), id));
            }
            Some(Incoming::Frame(Control::Leave { reason, .. })) => {
                leave = Some((entered.elapsed(), reason));
            }
            Some(_) => {}
            None => break,
        }
    }
    let (left_at, reason) = leave.expect("a silence release");
    assert_eq!(reason, LeaveReason::Silence.code());
    let first = probes.first().expect("probes").0;
    assert!(
        first >= Duration::from_millis(180) && first < Duration::from_millis(450),
        "first probe at {first:?}"
    );
    let after = probes.iter().filter(|(t, _)| *t > left_at).count();
    assert!(
        after >= 2,
        "suspect probes after the release at {left_at:?}: {probes:?}"
    );
    let fp = r.fp.clone();
    let st = r.status();
    assert_eq!(peer(&st, &fp).unwrap()["suspect"], true, "{st}");
    assert_eq!(
        st["capture"]["gate"], "idle",
        "No barrier at a suspect peer: {st}"
    );

    // One Pong: suspect clears.
    let (_, id) = *probes.last().unwrap();
    r.p.send_datagram(Datagram::Pong { id, t_us: 0 });
    let ponged = std::time::Instant::now();
    let c = &mut r.c;
    wait_for("suspect cleared", || {
        let st = c.result("status", json!(null));
        (peer(&st, &fp)?["suspect"] == false).then_some(())
    });
    assert!(
        ponged.elapsed() < Duration::from_millis(500),
        "{:?}",
        ponged.elapsed()
    );
    // The suspect peer's only edge had no barrier; PeerAlive puts it back.
    r.wait_gate("armed", None);
    // And probing stops.
    let quiet_from = std::time::Instant::now() + Duration::from_millis(150);
    let until = quiet_from + Duration::from_millis(500);
    while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
        if let Some(Incoming::Datagram(Datagram::Ping { .. })) = r.rt.block_on(r.p.next(left))
            && std::time::Instant::now() > quiet_from
        {
            panic!("probed after suspect cleared");
        }
    }
    let st = r.status();
    assert!(
        st["counters"]["stall_probes_started"].as_u64() >= Some(1),
        "{st}"
    );
    r.shutdown();
}

/// B-01, B-03 (stream B's flood, the source side): the daemon crosses into the fake, which never
/// acks and floods `Monitors`, two sets in turn. net used to await ctl's queue for each one,
/// which kept that queue full, and the capture thread's portal `Release` was dropped on `Full`:
/// the machine went local while the compositor kept capturing. The `Release` reaches the portal
/// 650 ms after the `Enter`, as without the flood.
#[test]
fn a_monitors_flood_does_not_keep_capture_on() {
    let mut r = Rig::new(&[]);
    let entered = r.cross();
    let until = entered + Duration::from_millis(1500);
    for m in monitors_pair().iter().cycle() {
        if std::time::Instant::now() >= until {
            break;
        }
        if r.p.send(m).is_err() {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    let released = r.release_after(entered) - entered;
    assert!(
        released <= Duration::from_millis(650 + 100),
        "Release {released:?} after the Enter"
    );
    let st = r.status();
    assert_eq!(st["role"], "idle", "{st}");
    r.shutdown();
}

/// A portal `Release` is never dropped. ctl's queue is full and
/// its main loop does not read; the crossing ends for silence (the fake never acks), and the
/// `Release` still reaches the portal on time, because portal commands have a channel of their
/// own that only the capture thread fills.
#[test]
fn a_release_survives_a_full_ctl_queue() {
    let mut r = Rig::new(&[]);
    let entered = r.cross();
    r.daemon().pause_ctl(Duration::from_secs(3));
    assert!(r.daemon().fill_ctl_queue() > 0);
    let released = r.release_after(entered) - entered;
    assert!(
        released <= Duration::from_millis(650 + 100),
        "Release {released:?} after the Enter"
    );
    r.shutdown();
}

/// On a capturing peer a session's end syncs and drops the devices, now on
/// ferry-inject's owner thread. A sync that takes 500 ms (a slow compositor) used to stall net
/// for as long, inside `target_event`; now net answers a `Ping` at once, and the barriers still
/// come back only after the devices are gone. Each session makes new devices with its keymap.
#[test]
fn a_slow_device_drop_holds_the_barriers_not_net() {
    let fake = FakeDevices::new();
    let mut r = Rig::with_devices(&[], &fake);
    fake.set_sync_delay(Duration::from_millis(500));
    for round in 0..2u16 {
        let from = r.portal.commands().len();
        r.p.keymap("xkb_keymap { fake };").unwrap();
        r.p.enter(523.0, 0.0).unwrap();
        r.frame_where("HeldAck#1", |m| matches!(m, Control::HeldAck { n: 1, .. }));
        r.wait_gate("idle", Some("controlled"));
        r.p.key(30 + round, true).unwrap();
        r.p.key(30 + round, false).unwrap();
        wait_for("the keys at the devices", || {
            (fake.commands().len() >= 3).then_some(())
        });

        // `left` is taken before the `Leave` goes out. The fake's 500 ms run from the moment net
        // asks for the sync, which is after the frame is sent; taken after the send, `left` was
        // late whenever the host descheduled this thread in between, and `drop_at - left` below
        // came out under 500 ms.
        let left = std::time::Instant::now();
        r.p.leave(LeaveReason::Home).unwrap();
        r.p.send_datagram(Datagram::Ping {
            id: 40 + u32::from(round),
            t_us: 0,
        });
        let pong_at = left + pong_after(&mut r, 40 + u32::from(round), left);
        // net answered while the sync was still on. The fake logs `Sync` when its 500 ms are up
        // and `Drop` after it, so a Pong received before either was logged was sent before the
        // sync was done: the order of two events, with no threshold on how fast the host is.
        let off_reason = r.status()["capture"]["off_reason"].clone();
        let done_at = fake
            .timed()
            .into_iter()
            .find_map(|(t, c)| matches!(c, DeviceCall::Sync | DeviceCall::Drop).then_some(t));
        assert!(
            done_at.is_none_or(|t| pong_at < t),
            "net answered only after the sync was done"
        );
        // And the barriers stay away while the devices exist. The status above was read before
        // the log: if the sync was still not done after it, it was not done when it answered.
        if done_at.is_none() {
            assert_eq!(off_reason, "controlled");
        }

        r.wait_gate("armed", None);
        r.portal_ran(from, &PortalCmd::Enable);
        let drop_at = last_drop(&fake);
        assert!(drop_at - left >= Duration::from_millis(500));
        let (set_at, _) = r
            .portal
            .timed()
            .into_iter()
            .rev()
            .find(|(_, c)| matches!(c, PortalCmd::SetBarriers { .. }))
            .expect("barriers again");
        assert!(set_at >= drop_at, "barriers before the devices were gone");
        assert_eq!(
            fake.calls(),
            [
                DeviceCall::Open(Some("xkb_keymap { fake };".into())),
                DeviceCall::Cmd("Motion { x: 523.0, y: 0.0 }".into()),
                DeviceCall::Cmd(format!("Key {{ code: {}, pressed: true }}", 30 + round)),
                DeviceCall::Cmd(format!("Key {{ code: {}, pressed: false }}", 30 + round)),
                DeviceCall::Cmd("ReleaseAll".into()),
                DeviceCall::Sync,
                DeviceCall::Drop,
            ],
            "round {round}"
        );
        fake.clear();
    }
    r.shutdown();
}

/// A new session that comes while the last one's devices are still being dropped: the
/// `Controlled(false)` that waited for them is void, so capture never re-arms in between (no
/// barrier while a virtual keyboard may exist), and it re-arms once, after the new session.
#[test]
fn a_session_before_the_drop_is_done_keeps_capture_off() {
    let fake = FakeDevices::new();
    let mut r = Rig::with_devices(&[], &fake);
    fake.set_sync_delay(Duration::from_millis(600));
    let from = r.portal.commands().len();
    r.p.keymap("xkb_keymap { fake };").unwrap();
    r.p.enter(523.0, 0.0).unwrap();
    r.frame_where("HeldAck#1", |m| matches!(m, Control::HeldAck { n: 1, .. }));
    r.wait_gate("idle", Some("controlled"));
    r.p.key(30, true).unwrap();
    wait_for("the key at the devices", || {
        fake.commands()
            .iter()
            .any(|c| c.contains("code: 30"))
            .then_some(())
    });
    r.p.leave(LeaveReason::Home).unwrap();
    // At once, before the 600 ms sync is done.
    r.p.enter(523.0, 0.0).unwrap();
    r.frame_where("HeldAck#1 again", |m| {
        matches!(m, Control::HeldAck { n: 1, .. })
    });
    wait_for("the first drop done", || {
        fake.calls().contains(&DeviceCall::Drop).then_some(())
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(r.status()["capture"]["off_reason"], "controlled");
    assert_eq!(
        r.portal.commands()[from..],
        [PortalCmd::Disable],
        "capture re-armed between the sessions"
    );

    r.p.leave(LeaveReason::Home).unwrap();
    r.wait_gate("armed", None);
    let after = r.portal_ran(from, &PortalCmd::Enable);
    assert_eq!(
        after
            .iter()
            .filter(|c| matches!(c, PortalCmd::Enable))
            .count(),
        1,
        "{after:?}"
    );
    let (set_at, _) = r
        .portal
        .timed()
        .into_iter()
        .rev()
        .find(|(_, c)| matches!(c, PortalCmd::SetBarriers { .. }))
        .expect("barriers again");
    assert!(
        set_at >= last_drop(&fake),
        "barriers before the devices were gone"
    );
    r.shutdown();
}

/// The shutdown tells the capture thread to stop even when that thread's queue is
/// full. A slow thread leaves it so after a burst of topology pushes, as the end of
/// `a_monitors_flood_does_not_keep_capture_on` does. ctl used to `try_send` the `Shutdown` once
/// and drop it on `Full`, and the join that followed never returned: `Daemon::shutdown` hung (a
/// run of that test under load stopped for over 500 s), and the shutdown's `Leave` and portal
/// `Release` were never sent. Here the thread sleeps 1.5 s (the daemon's `stall` hook) while its
/// queue is filled, and the shutdown starts at once.
#[test]
fn a_full_capture_queue_does_not_lose_the_shutdown() {
    let mut r = Rig::new(&[]);
    r.daemon()
        .stall(Thread::Capture, Duration::from_millis(1500));
    // A first event wakes the thread into its stall; the queue then fills behind it. The loop
    // ends when the queue is full and no longer drained: the stall.
    assert!(r.portal.event(SourceEvent::BatchEnd));
    let mut queued = 0;
    while r.portal.event(SourceEvent::BatchEnd) {
        queued += 1;
        assert!(queued < 10_000, "the capture thread never stalled");
    }
    let d = r.d.take().unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        d.shutdown();
        let _ = done_tx.send(());
    });
    assert!(
        done_rx.recv_timeout(20 * SECOND).is_ok(),
        "the shutdown did not return: the capture thread was never told to stop"
    );
}

/// A crossing over a linked edge reaches an `edge` subscriber on both sides of the
/// daemon, out and home, with the stretch of HDMI-A-1's bottom edge that links to the fake
/// (x ∈ [277, 1643), less the 8 px dead corners); `crossing.edge_glow = false` silences it.
#[test]
fn a_crossing_lights_the_edge_out_and_home() {
    let mut r = Rig::new(&[]);
    let mut glow = Client::connect(r.daemon().socket_path());
    glow.subscribe(&["edge"]);
    let edge = |glow: &mut Client, what: &str| {
        glow.event_within("edge", 3 * SECOND)
            .unwrap_or_else(|| panic!("no edge event for {what}"))["data"]
            .clone()
    };
    let want = |role: &str, home: bool| {
        json!({"role": role, "home": home, "peer": "fake", "monitor": "HDMI-A-1",
               "side": "bottom", "start": 285, "end": 1635})
    };

    // The fake drives the daemon: it comes in on the bottom row of HDMI-A-1, and goes home.
    r.p.keymap("xkb_keymap { fake };").unwrap();
    r.p.enter(800.0, 1079.0).unwrap();
    assert_eq!(edge(&mut glow, "the Enter"), want("target", false));
    r.p.leave(LeaveReason::Home).unwrap();
    assert_eq!(edge(&mut glow, "the Leave"), want("target", true));
    r.wait_gate("armed", None);

    // The daemon drives the fake over the same edge, and comes back up over it.
    r.cross();
    assert_eq!(edge(&mut glow, "the crossing"), want("source", false));
    assert!(r.portal.event(SourceEvent::Motion { dx: 0.0, dy: -40.0 }));
    assert_eq!(edge(&mut glow, "the way home"), want("source", true));
    r.wait_gate("armed", None);

    // Off: the same crossing sends nothing.
    r.c.result(
        "config.set",
        json!({"key": "crossing.edge_glow", "value": false}),
    );
    r.p.enter(800.0, 1079.0).unwrap();
    r.frame_where("HeldAck#1", |m| matches!(m, Control::HeldAck { n: 1, .. }));
    assert_eq!(glow.event_within("edge", SECOND / 2), None);
    r.shutdown();
}

/// `go down` warps and nudges through the linked edge, the nudge's activation crosses at
/// once despite the edge resistance, and the go chord on the fake brings control home to where
/// the cursor was. Both count in `go_crossings`; a side with no machine counts in `go_refused`.
#[test]
fn go_crosses_by_key_and_the_go_chord_comes_home() {
    let mut r = Rig::new(&[]);
    r.c.result(
        "config.set",
        json!({"key": "crossing.edge_resistance_px", "value": 150}),
    );
    assert!(r.portal.event(SourceEvent::Keymap {
        text: "xkb_keymap { desk };".into()
    }));
    r.frame_where("the daemon's keymap", |m| {
        matches!(m, Control::Keymap { .. })
    });
    r.portal.set_cursor(Some((100.0, 100.0)));

    assert_eq!(r.c.result("go", json!({"side": "left"})), json!({}));
    assert_eq!(r.c.result("go", json!({"side": "down"})), json!({}));
    let nudge = wait_for("the nudge", || r.portal.nudges().first().copied());
    assert_eq!(nudge, (960.0, 1077.0, 0.0, 4.0));
    assert!(r.portal.event(SourceEvent::StartEmulating));
    assert!(r.portal.event(SourceEvent::Activated {
        activation_id: 10,
        barrier_id: 1,
        x: nudge.0 + nudge.2,
        y: nudge.1 + nudge.3,
        lock: ferry_core::source::LockCheck::Unlocked,
    }));
    r.frame_where("the daemon's Enter", |m| matches!(m, Control::Enter { .. }));

    let since = std::time::Instant::now();
    for code in [125, 29, 56, 103] {
        assert!(r.portal.event(SourceEvent::Key {
            code,
            pressed: true,
            t_eis: ferry_core::time::Micros::ZERO,
        }));
    }
    let leave = r.frame_where("the Leave", |m| matches!(m, Control::Leave { .. }));
    assert!(matches!(leave, Control::Leave { reason, .. } if reason == LeaveReason::Home.code()));
    r.release_after(since);
    let home = r.portal.commands().into_iter().rev().find_map(|c| match c {
        PortalCmd::Release { pos, .. } => pos,
        _ => None,
    });
    assert_eq!(home, Some(ferry_core::geom::Point { x: 100.0, y: 100.0 }));
    let st = r.wait_gate("armed", None);
    assert_eq!(st["counters"]["go_crossings"], 2);
    assert_eq!(st["counters"]["go_refused"], 1);
    r.shutdown();
}
