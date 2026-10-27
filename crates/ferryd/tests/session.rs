//! A whole target session over a real ferry-net connection, with a fake source on loopback and a
//! recorder in place of ferry-inject. Network only: nothing here captures or injects.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::time::{Duration, Instant};

use ferry_core::target::InjectCmd;
use ferry_core::wire::{Control, Datagram, LeaveReason};
use ferry_net::{CloseCode, Identity};
use ferryd::fake_peer::{FakePeer, Incoming, hello, hello_now, monitor};
use ferryd::{Daemon, Recorder};
use serde_json::{Value, json};
use support::*;

const KEYMAP: &str = "xkb_keymap { fake };";
const SECOND: Duration = Duration::from_secs(1);
/// How long net may take to read the backlog of a flood: what the stream's flow-control window
/// holds (quinn's 1.25 MB, some 10,000 of these frames), at a pace that depends on the host.
/// 3 to 15 s was seen on a loaded one, and 64 s once. The bound only tells a hang from a slow
/// host.
const DRAIN: Duration = Duration::from_secs(120);

fn peer<'a>(status: &'a Value, fp: &str) -> Option<&'a Value> {
    status["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["fingerprint"] == fp)
}

/// A daemon that pins a fake source, and the fake connected with both Hellos in.
struct Rig {
    _env: Env,
    d: Option<Daemon>,
    c: Client,
    rt: tokio::runtime::Runtime,
    p: FakePeer,
    fp: String,
    rec: Recorder,
}

impl Rig {
    fn new(monitors: Vec<ferry_core::geom::MonitorInfo>) -> Self {
        let env = Env::new();
        let rec = Recorder::new();
        let d = start_recording(&env, &rec);
        let mut c = Client::connect(d.socket_path());
        let id = Identity::ephemeral().unwrap();
        let fp = id.fingerprint().to_string();
        c.result("trust", json!({"fingerprint": fp, "name": "fake"}));
        let rt = rt();
        let (addr, dfp) = (d.net_addr(), d.fingerprint());
        let mut p = rt.block_on(FakePeer::dial(id, addr, dfp)).unwrap();
        let theirs = rt
            .block_on(p.exchange_hello(&hello_now("fake", monitors)))
            .expect("the daemon answers Hello");
        match theirs {
            Control::Hello {
                proto_major,
                proto_minor,
                name,
                monitors,
                ..
            } => {
                assert_eq!((proto_major, proto_minor), (1, 0));
                assert_eq!(name, "desk");
                assert_eq!(monitors, desk_monitors());
            }
            other => panic!("{other:?}"),
        }
        wait_for("the peer up", || {
            let st = c.result("status", json!(null));
            (peer(&st, &fp)?["state"] == "connected").then_some(())
        });
        Rig {
            _env: env,
            d: Some(d),
            c,
            rt,
            p,
            fp,
            rec,
        }
    }

    fn frame(&mut self) -> Control {
        self.rt
            .block_on(self.p.next_frame(2 * SECOND))
            .expect("a frame")
    }

    /// Waits until the recorder holds `want`, and returns everything recorded.
    fn recorded(&self, what: &str, want: impl Fn(&[InjectCmd]) -> bool) -> Vec<InjectCmd> {
        wait_for(what, || {
            let all = self.rec.commands();
            want(&all).then_some(all)
        })
    }

    /// Keymap and Enter: an entered session on the daemon (the target).
    fn enter(&mut self, x: f32, y: f32) {
        self.p.keymap(KEYMAP).unwrap();
        self.p.enter(x, y).unwrap();
        let ack = self.frame();
        assert_eq!(
            ack,
            Control::HeldAck {
                session: self.p.session,
                n: 1
            }
        );
    }

    fn status(&mut self) -> Value {
        self.c.result("status", json!(null))
    }
}

#[test]
fn a_major_version_mismatch_is_refused_and_both_versions_are_shown() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let id = Identity::ephemeral().unwrap();
    let fp = id.fingerprint().to_string();
    c.result("trust", json!({"fingerprint": fp, "name": "future"}));
    let rt = rt();
    let mut p = rt
        .block_on(FakePeer::dial(id, d.net_addr(), d.fingerprint()))
        .unwrap();
    p.send(&hello("future", 2, 0, vec![])).unwrap();

    // The daemon answers with its own Hello (so this side can show both versions), then
    // Error{1}, then closes with application code 1.
    let mut got = Vec::new();
    let reason = rt.block_on(async {
        loop {
            match p
                .next(5 * SECOND)
                .await
                .expect("an answer before the close")
            {
                Incoming::Frame(m) => got.push(m),
                Incoming::Closed(r) => return r,
                _ => {}
            }
        }
    });
    assert!(
        matches!(
            got[0],
            Control::Hello {
                proto_major: 1,
                proto_minor: 0,
                ..
            }
        ),
        "{got:?}"
    );
    assert_eq!(
        got[1],
        Control::Error {
            code: 1,
            message: "version mismatch: 1.0 vs 2.0".into()
        }
    );
    assert_eq!(reason.code(), Some(CloseCode::VERSION), "{reason}");

    let st = wait_for("the refusal in status", || {
        let st = c.result("status", json!(null));
        (peer(&st, &fp)?["state"] == "refused").then_some(st)
    });
    let pe = peer(&st, &fp).unwrap();
    assert_eq!(pe["detail"], "version mismatch: 1.0 vs 2.0");
    assert_eq!(pe["proto"], "2.0");
    // the human status says it too
    d.shutdown();
}

#[test]
fn a_frame_before_hello_closes_with_code_2() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let id = Identity::ephemeral().unwrap();
    c.result(
        "trust",
        json!({"fingerprint": id.fingerprint().to_string(), "name": "rude"}),
    );
    let rt = rt();
    let mut p = rt
        .block_on(FakePeer::dial(id, d.net_addr(), d.fingerprint()))
        .unwrap();
    p.key(30, true).unwrap();
    let reason = rt.block_on(async {
        loop {
            if let Incoming::Closed(r) = p.next(5 * SECOND).await.expect("a close") {
                return r;
            }
        }
    });
    assert_eq!(reason.code(), Some(CloseCode::PROTOCOL), "{reason}");
    d.shutdown();
}

#[test]
fn a_target_session_with_snapshots_acks_and_the_650_ms_watchdog() {
    let mut r = Rig::new(vec![monitor("Virtual-1", 1280, 800)]);
    // Start-up step 3: the modifier reset is the first thing injected.
    assert_eq!(r.rec.commands().first(), Some(&InjectCmd::ModifierReset));

    // Ping is answered with Pong at once, and never reaches the machine.
    r.p.send_datagram(Datagram::Ping { id: 7, t_us: 1234 });
    let pong = match r.rt.block_on(r.p.next(2 * SECOND)).expect("a pong") {
        Incoming::Datagram(d) => d,
        other => panic!("{other:?}"),
    };
    assert_eq!(pong, Datagram::Pong { id: 7, t_us: 1234 });

    // Keymap, Enter at (523, 0), Held#1 → HeldAck#1.
    r.enter(523.0, 0.0);
    r.recorded("the keymap and the entry point", |c| {
        c.contains(&InjectCmd::SetKeymap(KEYMAP.into()))
            && c.contains(&InjectCmd::Motion { x: 523.0, y: 0.0 })
    });
    let st = r.status();
    assert_eq!(st["role"], "target");
    assert_eq!(st["active_peer"]["name"], "fake");

    // Keys and a motion datagram.
    r.p.key(30, true).unwrap();
    r.p.key(30, false).unwrap();
    r.p.motion(600.0, 100.0);
    r.recorded("a, then the motion", |c| {
        c.contains(&InjectCmd::Key {
            code: 30,
            pressed: false,
        }) && c.contains(&InjectCmd::Motion { x: 600.0, y: 100.0 })
    });

    // The Held cadence: every Held is acknowledged with its n.
    r.p.key(42, true).unwrap(); // Shift down, held
    let mut last_held = Instant::now();
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(100));
        last_held = Instant::now();
        r.p.held(&[42], &[]).unwrap();
        let n = r.p.held_n;
        let s = r.p.session;
        assert_eq!(r.frame(), Control::HeldAck { session: s, n });
    }

    // The source goes silent with Shift held: 650 ms after the last Held the target releases
    // everything and says Ended(watchdog). Time it from just before the last Held was sent: the
    // target cannot have seen it earlier, so it cannot fire early, and the upper bound only has
    // to prove it is not a whole second late, whatever else the machine is doing.
    r.rec.clear();
    let ended = r.frame();
    let after = last_held.elapsed();
    assert_eq!(
        ended,
        Control::Ended {
            session: r.p.session,
            reason: LeaveReason::Watchdog.code()
        }
    );
    assert!(
        after >= Duration::from_millis(600) && after <= Duration::from_millis(650) + SECOND,
        "{after:?}"
    );
    assert!(r.rec.commands().contains(&InjectCmd::ReleaseAll));
    let st = r.status();
    assert_eq!(st["role"], "idle");
    assert_eq!(st["last_release"]["reason"], "watchdog");
    assert_eq!(st["counters"]["watchdog_releases"], 1);
    assert_eq!(st["counters"]["enters"], 1);

    // More frames for the session the watchdog ended: it already got its one Ended.
    r.rec.clear();
    r.p.key(30, true).unwrap();
    r.p.held(&[30], &[]).unwrap();
    assert_eq!(r.rt.block_on(r.p.next_frame(SECOND / 2)), None);
    // Frames for a session this target never ran (a source that kept going after a restart):
    // answered with one Ended(stale), however many come.
    r.p.session += 1;
    for _ in 0..3 {
        r.p.key(30, true).unwrap();
        r.p.held(&[30], &[]).unwrap();
    }
    assert_eq!(
        r.frame(),
        Control::Ended {
            session: r.p.session,
            reason: LeaveReason::Stale.code()
        }
    );
    assert_eq!(r.rt.block_on(r.p.next_frame(SECOND / 2)), None);
    // Ended answers the first of the six; the other five are counted as the net thread gets to
    // them, so poll the counter instead of trusting that the half second above was enough.
    wait_for("all six stale frames counted", || {
        let st = r.status();
        (st["counters"]["stale_frames"].as_u64()? >= 6).then_some(())
    });
    assert!(
        !r.rec
            .commands()
            .iter()
            .any(|c| matches!(c, InjectCmd::Key { .. })),
        "nothing of a stale session is injected"
    );
    r.d.take().unwrap().shutdown();
}

/// Sends `msgs`, in turn, for `span` as fast as the connection takes them. A full frame queue
/// just waits a little.
fn flood(r: &mut Rig, msgs: &[Control], span: Duration) {
    let until = Instant::now() + span;
    for msg in msgs.iter().cycle() {
        if Instant::now() >= until {
            return;
        }
        if r.p.send(msg).is_err() {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

/// Sends `count` of `msgs`, in turn, evenly over `span`: a steady stream that is still far
/// faster than ctl and net can pass on. Sent as fast as the connection takes them, they would
/// leave it a backlog of up to the stream's window (some 10,000 frames) and overflow the
/// loopback's UDP buffers while net is stalled; how long the daemon then needs depends on the
/// host (see `DRAIN`), and on a loaded one the connection could even be given up, none of which
/// is what the test checks. A frame queue that is full just waits.
fn paced(r: &mut Rig, msgs: &[Control], count: u32, span: Duration) {
    let start = Instant::now();
    for (k, msg) in (0..count).zip(msgs.iter().cycle()) {
        if let Some(wait) = (start + span * k / count).checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        wait_for_within("room in the frame queue", DRAIN, || r.p.send(msg).ok());
    }
}

/// Two `Monitors` frames whose sets differ, so ctl takes each one as a change.
fn monitors_pair() -> [Control; 2] {
    [
        Control::Monitors {
            monitors: vec![monitor("Virtual-1", 1280, 800)],
        },
        Control::Monitors {
            monitors: vec![monitor("Virtual-1", 1366, 768)],
        },
    ]
}

/// The next `Pong` for `id` (within 10 s: after a flood it waits for the frames before it),
/// skipping everything else, and how long after `since` it came.
fn pong_after(r: &mut Rig, id: u32, since: Instant) -> Duration {
    let until = since + 10 * SECOND;
    loop {
        let left = until
            .checked_duration_since(Instant::now())
            .unwrap_or_else(|| panic!("no Pong {id} within 10 s"));
        match r.rt.block_on(r.p.next(left)) {
            Some(Incoming::Datagram(Datagram::Pong { id: got, .. })) if got == id => {
                return since.elapsed();
            }
            Some(Incoming::Closed(why)) => panic!("closed before Pong {id}: {why:?}"),
            Some(_) => {}
            None => panic!("no Pong {id} within 10 s"),
        }
    }
}

/// When the recorder got its first `ReleaseAll` at or after `since`.
fn release_all_after(rec: &Recorder, since: Instant) -> Option<Instant> {
    rec.timed()
        .into_iter()
        .find(|(t, c)| *t >= since && *c == InjectCmd::ReleaseAll)
        .map(|(t, _)| t)
}

/// The watchdog's `Tick` fires on time while the net loop always has a frame
/// ready. A test hook makes the loop wait 1 ms after each event, as a handler that awaits another
/// thread does, so a flood of `LayoutAck` frames (net ignores them; none reaches the
/// TargetMachine, whose own events would fire an expired watchdog) stays queued behind it. The
/// source's last `Held` holds Shift. The loop used to rebuild its `sleep_until` every iteration,
/// so the `Tick` never won `select!` and the key stayed down until the flood ended.
#[test]
fn the_watchdog_fires_during_a_frame_backlog() {
    let mut r = Rig::new(vec![monitor("Virtual-1", 1280, 800)]);
    r.enter(523.0, 0.0);
    r.p.key(42, true).unwrap();
    r.d.as_ref().unwrap().drag_net(Duration::from_millis(1));
    let last_held = Instant::now();
    r.p.held(&[42], &[]).unwrap();
    let ack = Control::LayoutAck {
        rev: ferry_core::wire::control::WireRevision {
            counter: 1,
            peer: [0; 32],
        },
    };
    flood(&mut r, &[ack], Duration::from_millis(1500));

    let released = wait_for("the watchdog's ReleaseAll", || {
        release_all_after(&r.rec, last_held)
    });
    r.d.as_ref().unwrap().drag_net(Duration::ZERO);
    let after = released - last_held;
    assert!(
        after >= Duration::from_millis(600) && after <= Duration::from_millis(650 + 50),
        "ReleaseAll {after:?} after the last Held"
    );
    let session = r.p.session;
    let ended = loop {
        match r.rt.block_on(r.p.next_frame(3 * SECOND)) {
            Some(m @ Control::Ended { .. }) => break m,
            Some(_) => {}
            None => panic!("no Ended"),
        }
    };
    assert_eq!(
        ended,
        Control::Ended {
            session,
            reason: LeaveReason::Watchdog.code()
        }
    );
    r.d.take().unwrap().shutdown();
}

/// B-03 (stream B's flood, the target side): a trusted source holds Shift, stops sending `Held`
/// and floods `Monitors`, two sets in turn, so ctl saves `layout.toml` for each. net used to
/// await ctl's queue for every one: it fell behind its own queue, and a `Ping` sent after
/// the flood was answered about 2 s later. The key is released 650 ms after the last `Held`.
/// The `Ping` still waits for the frames the connection had already taken in (a few hundred ms
/// in a debug build, as for a flood of `LayoutAck`s that net ignores), so only its answer is
/// checked here; `net_answers_while_ctl_never_reads` times it without a flood.
#[test]
fn a_monitors_flood_does_not_hold_a_key_down() {
    let mut r = Rig::new(vec![monitor("Virtual-1", 1280, 800)]);
    r.enter(523.0, 0.0);
    r.p.key(42, true).unwrap();
    let last_held = Instant::now();
    r.p.held(&[42], &[]).unwrap();
    flood(&mut r, &monitors_pair(), Duration::from_millis(1500));
    let pinged = Instant::now();
    r.p.send_datagram(Datagram::Ping { id: 77, t_us: 0 });

    let released = wait_for("the watchdog's ReleaseAll", || {
        release_all_after(&r.rec, last_held)
    });
    let after = released - last_held;
    assert!(
        after >= Duration::from_millis(600) && after <= Duration::from_millis(650 + 50),
        "ReleaseAll {after:?} after the last Held"
    );
    pong_after(&mut r, 77, pinged);
    // ctl saves a burst of monitors once it settles (in memory at once): the last set sent is
    // the one on disk.
    r.p.send(&Control::Monitors {
        monitors: vec![monitor("Last-1", 1024, 768)],
    })
    .unwrap();
    let file = r._env.paths().layout_file();
    wait_for("the last monitors on disk", || {
        std::fs::read_to_string(&file)
            .ok()?
            .contains("Last-1")
            .then_some(())
    });
    r.d.take().unwrap().shutdown();
}

/// Net never waits for ctl. With ctl's queue full and its loop not reading,
/// a peer's `Monitors` and `Layout` frames still leave net free: it answers `Ping`s and acks
/// `Held` at once, and the frames reach ctl once it reads again.
#[test]
fn net_answers_while_ctl_never_reads() {
    let mut r = Rig::new(vec![monitor("Virtual-1", 1280, 800)]);
    r.enter(10.0, 10.0);
    let d = r.d.as_ref().unwrap();
    d.pause_ctl(Duration::from_secs(3));
    assert!(d.fill_ctl_queue() > 0);
    for m in monitors_pair().iter().cycle().take(20) {
        r.p.send(m).unwrap();
    }
    let pinged = Instant::now();
    r.p.send_datagram(Datagram::Ping { id: 5, t_us: 0 });
    let pong = pong_after(&mut r, 5, pinged);
    assert!(pong < Duration::from_millis(300), "Pong after {pong:?}");
    let sent = Instant::now();
    r.p.held(&[], &[]).unwrap();
    let n = r.p.held_n;
    assert_eq!(
        r.frame(),
        Control::HeldAck {
            session: r.p.session,
            n
        }
    );
    assert!(
        sent.elapsed() < Duration::from_millis(300),
        "{:?}",
        sent.elapsed()
    );
    // `status` is served while the loop is paused; the last Monitors lands once it reads again.
    assert_eq!(r.status()["role"], "target");
    let fp = r.fp.clone();
    let c = &mut r.c;
    wait_for("the last Monitors at ctl", || {
        let v = c.result("layout.get", json!(null));
        let fake = v["peers"]
            .as_array()?
            .iter()
            .find(|p| p["fingerprint"] == fp)?
            .clone();
        (fake["monitors"][0]["w"] == 1366).then_some(())
    });
    r.d.take().unwrap().shutdown();
}

/// B-07 (RG-CONC-02 as amended, G-11): ctl and net never wait on each other's queues. ctl's
/// queue is full and its loop does not read; net stalls while 100 `release` requests queue up
/// for it; the source floods `Monitors` through all of it, and a second pinned peer comes up,
/// which makes ctl's main loop send it the layout. ctl used to await net there while net awaited
/// ctl for the `Monitors`: with both queues full, neither moved again. `status` answers
/// throughout, and after the stalls every request is answered and net answers `Ping`s again.
#[test]
fn both_queues_full_and_status_still_answers() {
    let mut r = Rig::new(vec![monitor("eDP-1", 1366, 768)]);
    let second = Identity::ephemeral().unwrap();
    r.c.result(
        "trust",
        json!({"fingerprint": second.fingerprint().to_string(), "name": "second"}),
    );
    r.c.result(
        "layout.place",
        json!({"peer": "fake", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
    );
    let d = r.d.as_ref().unwrap();
    let (addr, dfp, path) = (d.net_addr(), d.fingerprint(), d.socket_path().to_path_buf());
    d.pause_ctl(Duration::from_millis(1500));
    assert!(d.fill_ctl_queue() > 0);
    d.stall(ferryd::watchdog::Thread::Net, Duration::from_millis(500));
    r.p.send_datagram(Datagram::Ping { id: 1, t_us: 0 }); // wakes net into its stall

    let releases: Vec<_> = (0..100)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || Client::connect(&path).call("release", json!(null)))
        })
        .collect();
    let handle = r.rt.handle().clone();
    let second_up = std::thread::spawn(move || {
        handle.block_on(async move {
            let mut p2 = FakePeer::dial(second, addr, dfp).await.unwrap();
            p2.exchange_hello(&hello_now("second", vec![monitor("eDP-1", 800, 600)]))
                .await
                .expect("the second peer's Hello answered");
            // ctl's main loop sends it the layout once it reads again
            match p2.next_frame(8 * SECOND).await {
                Some(Control::Layout { .. }) => {}
                other => panic!("{other:?}"),
            }
            p2
        })
    });
    paced(&mut r, &monitors_pair(), 2000, Duration::from_millis(3000));
    let t0 = Instant::now();
    assert_eq!(r.status()["me"]["name"], "desk");
    assert!(
        t0.elapsed() < Duration::from_millis(300),
        "status after {:?}",
        t0.elapsed()
    );

    for h in releases {
        let resp = h.join().expect("every release answered");
        assert!(
            resp.get("result").is_some() || resp.get("error").is_some(),
            "{resp}"
        );
    }
    let _p2 = second_up
        .join()
        .expect("the second peer up, with the layout");
    // Net answers `Ping`s again. The flood's frames wait in the connection ahead of every
    // datagram (net's reader takes frames first), and how long net needs to read them depends
    // on the host: a `Ping` sent at once came back 3 to 10 s late on a loaded one, and sometimes
    // past `pong_after`'s bound. So it follows a last `Monitors`, which is in the same stream
    // behind the whole backlog: once ctl has it, net has read the rest. (Until the backlog is
    // read the stream is full, and the frame does not even fit the sender's queue.)
    let last = Control::Monitors {
        monitors: vec![monitor("Last-1", 1024, 768)],
    };
    wait_for_within("room for the last Monitors", DRAIN, || r.p.send(&last).ok());
    let fp = r.fp.clone();
    let c = &mut r.c;
    wait_for_within("the flood read to its end", DRAIN, || {
        let v = c.result("layout.get", json!(null));
        let fake = v["peers"]
            .as_array()?
            .iter()
            .find(|p| p["fingerprint"] == fp)?
            .clone();
        (fake["monitors"][0]["w"] == 1024).then_some(())
    });
    let pinged = Instant::now();
    r.p.send_datagram(Datagram::Ping { id: 2, t_us: 0 });
    pong_after(&mut r, 2, pinged);
    r.d.take().unwrap().shutdown();
}

#[test]
fn ferry_release_ends_the_session_as_forced() {
    let mut r = Rig::new(vec![]);
    r.enter(10.0, 10.0);
    r.p.key(125, true).unwrap(); // Super held
    r.recorded("Super down", |c| {
        c.contains(&InjectCmd::Key {
            code: 125,
            pressed: true,
        })
    });
    r.rec.clear();
    assert_eq!(r.c.result("release", json!(null)), json!({}));
    assert_eq!(
        r.frame(),
        Control::Ended {
            session: r.p.session,
            reason: LeaveReason::Forced.code()
        }
    );
    assert_eq!(r.rec.commands(), vec![InjectCmd::ReleaseAll]);
    let st = r.status();
    assert_eq!(st["role"], "idle");
    assert_eq!(st["last_release"]["reason"], "forced");
    assert_eq!(st["counters"]["releases"], 1);
    r.d.take().unwrap().shutdown();
}

/// The inject backend dies in the middle of a session (its
/// thread ends on a fatal Wayland error). The session used to go on, with every later command
/// lost and the source capturing. Now it ends as forced: the source gets `Ended` and releases,
/// `status` shows it, and the next session gets new devices.
#[test]
fn a_dead_inject_thread_ends_the_session() {
    let mut r = Rig::new(vec![]);
    r.enter(10.0, 10.0);
    r.p.key(42, true).unwrap(); // Shift held
    r.recorded("Shift down", |c| {
        c.contains(&InjectCmd::Key {
            code: 42,
            pressed: true,
        })
    });
    r.rec.die("input thread is gone");
    r.p.key(30, true).unwrap();
    let session = r.p.session;
    assert_eq!(
        r.frame(),
        Control::Ended {
            session,
            reason: LeaveReason::Forced.code()
        }
    );
    let st = r.status();
    assert_eq!(st["role"], "idle");
    assert_eq!(st["last_release"]["reason"], "forced");
    assert_eq!(st["counters"]["inject_failures"], 1);
    assert!(
        r.rec
            .device_calls()
            .last()
            .is_some_and(|(_, c)| *c == "drop"),
        "the dead devices are dropped"
    );

    // The next session runs on new devices.
    r.rec.clear();
    r.enter(20.0, 20.0);
    r.p.key(30, true).unwrap();
    r.recorded("a key in the next session", |c| {
        c.contains(&InjectCmd::Key {
            code: 30,
            pressed: true,
        })
    });
    r.d.take().unwrap().shutdown();
}

#[test]
fn a_lost_link_ends_the_session_as_link_lost() {
    let mut r = Rig::new(vec![]);
    r.enter(10.0, 10.0);
    r.p.key(56, true).unwrap(); // Alt held
    r.recorded("Alt down", |c| {
        c.contains(&InjectCmd::Key {
            code: 56,
            pressed: true,
        })
    });
    r.rec.clear();
    // The source goes away with no Leave (a kill or a stateless reset in the exit check).
    r.p.conn.close(CloseCode::SHUTDOWN, "gone");
    r.recorded("the release", |c| c.contains(&InjectCmd::ReleaseAll));
    let st = wait_for("the session ended", || {
        let st = r.status();
        (st["role"] == "idle").then_some(st)
    });
    assert_eq!(st["last_release"]["reason"], "link_lost");
    r.d.take().unwrap().shutdown();
}

#[test]
fn shutdown_sends_ended_shutdown_before_the_close() {
    let mut r = Rig::new(vec![]);
    r.enter(10.0, 10.0);
    r.p.key(29, true).unwrap(); // Ctrl held
    r.recorded("Ctrl down", |c| {
        c.contains(&InjectCmd::Key {
            code: 29,
            pressed: true,
        })
    });
    r.rec.clear();
    let d = r.d.take().unwrap();
    let stopper = std::thread::spawn(move || d.shutdown());
    let mut seen = Vec::new();
    let reason = r.rt.block_on(async {
        loop {
            match r.p.next(5 * SECOND).await.expect("frames, then the close") {
                Incoming::Frame(m) => seen.push(m),
                Incoming::Closed(reason) => return reason,
                _ => {}
            }
        }
    });
    stopper.join().unwrap();
    assert_eq!(
        seen,
        vec![Control::Ended {
            session: r.p.session,
            reason: LeaveReason::Shutdown.code()
        }]
    );
    assert_eq!(reason.code(), Some(CloseCode::SHUTDOWN), "{reason}");
    assert!(reason.by_peer(), "{reason}");
    assert_eq!(r.rec.commands().first(), Some(&InjectCmd::ReleaseAll));
}

#[test]
fn the_layout_is_pushed_and_the_higher_revision_is_kept() {
    let mut r = Rig::new(vec![monitor("eDP-1", 1366, 768)]);
    // Nothing placed yet: nothing pushed at peer up.
    assert_eq!(r.rt.block_on(r.p.next_frame(SECOND / 4)), None);

    // A local place pushes the whole layout at revision (1, desk).
    let placed = r.c.result(
        "layout.place",
        json!({"peer": "fake", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
    );
    assert_eq!(placed["revision"]["counter"], 1);
    let pushed = match r.frame() {
        m @ Control::Layout { .. } => ferry_core::geom::Layout::from_wire(&m).unwrap(),
        other => panic!("{other:?}"),
    };
    assert_eq!(pushed.rev.counter, 1);
    let fake = ferry_core::PeerId::parse(&r.fp).unwrap();
    assert_eq!(pushed.origin(fake), Some((277, 1080)));

    // A higher revision from the peer is kept and saved.
    let mut higher = pushed.clone();
    higher.rev = ferry_core::geom::Revision {
        counter: 5,
        peer: fake,
    };
    higher.set_origin(ferry_core::geom::Origin {
        peer: fake,
        x: 0,
        y: 1080,
    });
    r.p.send(&higher.to_wire()).unwrap();
    let view = wait_for("the pushed layout kept", || {
        let v = r.c.result("layout.get", json!(null));
        (v["revision"]["counter"] == 5).then_some(v)
    });
    assert_eq!(view["revision"]["peer"], r.fp);
    let fake_view = view["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "fake")
        .unwrap()
        .clone();
    assert_eq!(fake_view["origin"], json!([0, 1080]));
    let file = std::fs::read_to_string(r._env.paths().layout_file()).unwrap();
    assert!(file.contains("counter = 5"), "{file}");
    // The daemon holds a revision the peer made itself, and says so by sending it back
    // (M3 design 4.3).
    match r.frame() {
        m @ Control::Layout { .. } => {
            let echoed = ferry_core::geom::Layout::from_wire(&m).unwrap();
            assert_eq!(echoed, higher);
        }
        other => panic!("{other:?}"),
    }

    // A lower one is ignored.
    let mut lower = higher.clone();
    lower.rev.counter = 2;
    lower.set_origin(ferry_core::geom::Origin {
        peer: fake,
        x: 500,
        y: 1080,
    });
    r.p.send(&lower.to_wire()).unwrap();
    // A sentinel (G-26): ctl takes the peer's frames in order, so once a `Monitors` sent after
    // the lower layout shows up on disk, the layout has been looked at and refused.
    r.p.send(&Control::Monitors {
        monitors: vec![monitor("Sentinel-1", 1024, 768)],
    })
    .unwrap();
    let file = r._env.paths().layout_file();
    wait_for("the sentinel monitors on disk", || {
        std::fs::read_to_string(&file)
            .ok()?
            .contains("Sentinel-1")
            .then_some(())
    });
    let v = r.c.result("layout.get", json!(null));
    assert_eq!(v["revision"]["counter"], 5);
    let file = std::fs::read_to_string(&file).unwrap();
    assert!(
        file.contains("counter = 5") && !file.contains("x = 500"),
        "{file}"
    );

    // The next local edit goes past the highest revision seen, and is pushed.
    let placed = r.c.result(
        "layout.place",
        json!({"peer": "fake", "rel": "below", "anchor": "HDMI-A-1", "align": "start"}),
    );
    assert_eq!(placed["revision"]["counter"], 6);
    match r.frame() {
        m @ Control::Layout { .. } => {
            let l = ferry_core::geom::Layout::from_wire(&m).unwrap();
            assert_eq!(l.rev.counter, 6);
            assert_eq!(l.origin(fake), Some((0, 1080)));
        }
        other => panic!("{other:?}"),
    }
    r.d.take().unwrap().shutdown();
}

#[test]
fn a_peer_that_comes_up_gets_the_layout() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    // The same key twice: an identity on disk, loaded once per connection.
    let key_dir = env.tmp.path().join("nb-key");
    let id = Identity::load_or_create(&key_dir).unwrap();
    let fp = id.fingerprint().to_string();
    c.result("trust", json!({"fingerprint": fp, "name": "nb"}));
    let rt = rt();
    // First connection: the daemon learns the monitors; place it.
    let mut p = rt
        .block_on(FakePeer::dial(id, d.net_addr(), d.fingerprint()))
        .unwrap();
    rt.block_on(p.exchange_hello(&hello_now("nb", vec![monitor("eDP-1", 1366, 768)])))
        .unwrap();
    wait_for("nb up", || {
        let st = c.result("status", json!(null));
        (peer(&st, &fp)?["state"] == "connected").then_some(())
    });
    c.result(
        "layout.place",
        json!({"peer": "nb", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
    );
    p.conn.close(CloseCode::SHUTDOWN, "bye");
    wait_for("nb down", || {
        let st = c.result("status", json!(null));
        (peer(&st, &fp)?["state"] == "offline").then_some(())
    });
    // The peer goes away for good, as a restarted one does: the daemon learned its address from
    // the inbound connection and would otherwise dial this endpoint back.
    drop(p);

    // Second connection: the layout comes right after the Hello exchange.
    let mut p = rt
        .block_on(FakePeer::dial(
            Identity::load_or_create(&key_dir).unwrap(),
            d.net_addr(),
            d.fingerprint(),
        ))
        .unwrap();
    rt.block_on(p.exchange_hello(&hello_now("nb", vec![monitor("eDP-1", 1366, 768)])))
        .unwrap();
    match rt.block_on(p.next_frame(2 * SECOND)) {
        Some(m @ Control::Layout { .. }) => {
            let l = ferry_core::geom::Layout::from_wire(&m).unwrap();
            assert_eq!(l.rev.counter, 1);
            assert_eq!(
                l.origin(ferry_core::PeerId::parse(&fp).unwrap()),
                Some((277, 1080))
            );
        }
        other => panic!("{other:?}"),
    }
    d.shutdown();
}

/// A layout that the fake peer made, at `counter`, with the fake placed at `origin`.
fn layout_by(
    fake: ferry_core::PeerId,
    counter: u64,
    origin: (i32, i32),
) -> ferry_core::geom::Layout {
    let mut l = ferry_core::geom::Layout::empty(fake);
    l.rev = ferry_core::geom::Revision {
        counter,
        peer: fake,
    };
    l.set_origin(ferry_core::geom::Origin {
        peer: fake,
        x: origin.0,
        y: origin.1,
    });
    l
}

/// The revision of the next `Layout` frame the fake peer gets, skipping nothing else.
fn next_layout(r: &mut Rig) -> ferry_core::geom::Layout {
    match r.frame() {
        m @ Control::Layout { .. } => ferry_core::geom::Layout::from_wire(&m).unwrap(),
        other => panic!("{other:?}"),
    }
}

fn replaced(r: &mut Rig) -> u64 {
    r.status()["counters"]["layout_replaced"]
        .as_u64()
        .unwrap_or(0)
}

/// This peer's own edit lost to a higher revision from a peer that never held it.
/// The hook counts it (`layout_replaced`); the toast is wired to the hook later.
#[test]
fn an_edit_replaced_by_a_peer_that_never_held_it_is_a_lost_edit() {
    let mut r = Rig::new(vec![monitor("eDP-1", 1366, 768)]);
    r.c.result(
        "layout.place",
        json!({"peer": "fake", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
    );
    let ours = next_layout(&mut r);
    assert_eq!(ours.rev.counter, 1);
    assert_eq!(replaced(&mut r), 0);

    // two offline edits, then the reconnect: the peer's push is higher and says nothing of ours
    let fake = ferry_core::PeerId::parse(&r.fp).unwrap();
    r.p.send(&layout_by(fake, 5, (0, 1080)).to_wire()).unwrap();
    let c = &mut r.c;
    wait_for("the higher revision kept", || {
        (c.result("layout.get", json!(null))["revision"]["counter"] == 5).then_some(())
    });
    // ctl handles the push in one step: the hook has run when the new layout can be seen
    assert_eq!(replaced(&mut r), 1);
    // a second replacement is not another loss: the edit is gone
    r.p.send(&layout_by(fake, 6, (0, 1090)).to_wire()).unwrap();
    let c = &mut r.c;
    wait_for("revision 6", || {
        (c.result("layout.get", json!(null))["revision"]["counter"] == 6).then_some(())
    });
    assert_eq!(replaced(&mut r), 1);
    r.d.take().unwrap().shutdown();
}

/// A peer that held the edit, and then changed the layout, built on it: nothing was lost.
#[test]
fn a_change_made_on_top_of_our_edit_is_not_a_lost_edit() {
    let mut r = Rig::new(vec![monitor("eDP-1", 1366, 768)]);
    r.c.result(
        "layout.place",
        json!({"peer": "fake", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
    );
    let ours = next_layout(&mut r);
    // the peer keeps ours and says so: it sends the layout back, though it made nothing
    r.p.send(&ours.to_wire()).unwrap();
    // then it edits
    let fake = ferry_core::PeerId::parse(&r.fp).unwrap();
    r.p.send(&layout_by(fake, 2, (0, 1080)).to_wire()).unwrap();
    let c = &mut r.c;
    wait_for("the peer's edit kept", || {
        (c.result("layout.get", json!(null))["revision"]["counter"] == 2).then_some(())
    });
    assert_eq!(replaced(&mut r), 0);
    r.d.take().unwrap().shutdown();
}

/// The daemon answers a peer that pushes a revision it made itself, when it holds that revision
/// (the echo); it does not answer a lower one, or a copy of somebody else's revision.
#[test]
fn a_layout_is_sent_back_only_to_the_peer_that_made_it_and_only_when_held() {
    let mut r = Rig::new(vec![monitor("eDP-1", 1366, 768)]);
    let fake = ferry_core::PeerId::parse(&r.fp).unwrap();

    // kept, and made by the sender: echoed
    let five = layout_by(fake, 5, (0, 1080));
    r.p.send(&five.to_wire()).unwrap();
    assert_eq!(next_layout(&mut r), five);
    // the same revision again, still made by the sender: held, echoed again
    r.p.send(&five.to_wire()).unwrap();
    assert_eq!(next_layout(&mut r), five);

    // lower: not held, not answered. The edit that follows is the next frame the peer gets.
    r.p.send(&layout_by(fake, 2, (0, 5000)).to_wire()).unwrap();
    // higher, but made by a third peer, which the sender only relays: kept, not answered
    let third = ferry_core::PeerId([0x3c; 32]);
    let mut theirs = layout_by(third, 9, (0, 1080));
    theirs.set_origin(ferry_core::geom::Origin {
        peer: fake,
        x: 0,
        y: 1080,
    });
    r.p.send(&theirs.to_wire()).unwrap();
    let c = &mut r.c;
    wait_for("revision 9", || {
        (c.result("layout.get", json!(null))["revision"]["counter"] == 9).then_some(())
    });
    let placed = r.c.result(
        "layout.place",
        json!({"peer": "fake", "rel": "below", "anchor": "HDMI-A-1", "align": "start"}),
    );
    assert_eq!(placed["revision"]["counter"], 10);
    assert_eq!(
        next_layout(&mut r).rev.counter,
        10,
        "no answer came before the push"
    );
    r.d.take().unwrap().shutdown();
}

#[test]
fn two_texts_of_one_keymap_make_one_virtual_keyboard() {
    // A source that sends both texts of the desktop's keymap with a new serial each, as
    // 192f507 did when fcitx5's keyboard and the real one took turns: the target compares
    // canonical forms, so one keyboard serves them all. A real change still swaps.
    let names = |layout: &str, variant: &str, options: &str| ferry_inject::KeymapNames {
        layout: layout.into(),
        variant: variant.into(),
        options: Some(options.into()),
        ..Default::default()
    };
    let keyboard = ferry_inject::keymap_from_names(&names(
        "us",
        "intl",
        "compose:caps,shift:both_capslock_cancel",
    ))
    .unwrap();
    let fcitx5 = ferry_inject::canonical_keymap(&keyboard).unwrap();
    assert_ne!(keyboard, fcitx5);
    let br = ferry_inject::keymap_from_names(&names("br", "", "")).unwrap();

    let mut r = Rig::new(vec![monitor("Virtual-1", 1280, 800)]);
    r.p.keymap(&keyboard).unwrap();
    r.p.enter(10.0, 10.0).unwrap();
    assert!(matches!(r.frame(), Control::HeldAck { n: 1, .. }));
    for text in [&fcitx5, &keyboard, &fcitx5, &keyboard] {
        r.p.keymap(text).unwrap();
    }
    // Frames apply in order: once this key is injected, every keymap before it was handled.
    r.p.key(30, true).unwrap();
    let all = r.recorded("the key after the keymaps", |c| {
        c.contains(&InjectCmd::Key {
            code: 30,
            pressed: true,
        })
    });
    let swaps = |all: &[InjectCmd]| {
        all.iter()
            .filter_map(|c| match c {
                InjectCmd::SetKeymap(t) => Some(t.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(swaps(&all), std::slice::from_ref(&fcitx5));
    assert_eq!(r.status()["counters"]["keymap_swaps"], 1);

    r.p.keymap(&br).unwrap();
    r.p.key(30, false).unwrap();
    let all = r.recorded("the release after br", |c| {
        c.contains(&InjectCmd::Key {
            code: 30,
            pressed: false,
        })
    });
    assert_eq!(
        swaps(&all),
        [fcitx5, ferry_inject::canonical_keymap(&br).unwrap()]
    );
    assert_eq!(r.status()["counters"]["keymap_swaps"], 2);
}
