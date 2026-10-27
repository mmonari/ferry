//! The own-monitor watcher against a fake Hyprland: a runtime dir with `hypr/<sig>/.socket.sock`
//! (answers `j/monitors all`) and `.socket2.sock` (we write events into it). Nothing here
//! touches the real compositor.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferry_core::geom::WireOffset;
use ferry_core::wire::Control;
use ferry_net::Identity;
use ferryd::MonitorSource;
use ferryd::fake_peer::{FakePeer, hello_now, monitor};
use serde_json::{Value, json};
use support::*;

struct FakeHyprland {
    /// Every other command, with when it came; a `dispatch` is answered `ok`.
    commands: Arc<Mutex<Vec<(Instant, String)>>>,
    monitors: Arc<Mutex<String>>,
    reads: Arc<AtomicUsize>,
    /// When each `j/monitors all` arrived.
    read_times: Arc<Mutex<Vec<Instant>>>,
    events: Arc<Mutex<Option<UnixStream>>>,
}

fn monitors_json(list: &[(&str, u32, u32, i32)]) -> String {
    let items: Vec<String> = list
        .iter()
        .enumerate()
        .map(|(i, (name, w, h, x))| {
            format!(
                r#"{{"id":{i},"name":"{name}","description":"d {name}","width":{w},"height":{h},"x":{x},"y":0,"scale":1.0,"transform":0,"disabled":false}}"#
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

impl FakeHyprland {
    fn start(runtime: &std::path::Path, initial: &[(&str, u32, u32, i32)]) -> Self {
        let dir = runtime.join("hypr/fake_1_1");
        std::fs::create_dir_all(&dir).unwrap();
        let monitors = Arc::new(Mutex::new(monitors_json(initial)));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let reads = Arc::new(AtomicUsize::new(0));
        let read_times = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(None));

        let cmd = UnixListener::bind(dir.join(".socket.sock")).unwrap();
        let (m, r, rt, cmds) = (
            monitors.clone(),
            reads.clone(),
            read_times.clone(),
            commands.clone(),
        );
        std::thread::spawn(move || {
            for stream in cmd.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buf = [0u8; 256];
                // a liveness probe connects and closes without a command
                let Ok(n) = stream.read(&mut buf) else {
                    continue;
                };
                if n == 0 {
                    continue;
                }
                let command = String::from_utf8_lossy(&buf[..n]).to_string();
                if command.starts_with("j/monitors all") {
                    rt.lock().unwrap().push(Instant::now());
                    r.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(m.lock().unwrap().as_bytes());
                } else {
                    cmds.lock().unwrap().push((Instant::now(), command.clone()));
                    if command.starts_with("dispatch") {
                        let _ = stream.write_all(b"ok");
                    }
                }
            }
        });

        let ev = UnixListener::bind(dir.join(".socket2.sock")).unwrap();
        let slot = events.clone();
        std::thread::spawn(move || {
            for stream in ev.incoming().flatten() {
                *slot.lock().unwrap() = Some(stream);
            }
        });
        Self {
            commands,
            monitors,
            reads,
            read_times,
            events,
        }
    }

    fn set_monitors(&self, list: &[(&str, u32, u32, i32)]) {
        *self.monitors.lock().unwrap() = monitors_json(list);
    }

    fn emit(&self, line: &str) {
        let mut guard = self.events.lock().unwrap();
        let s = guard.as_mut().expect("the daemon subscribed to socket2");
        s.write_all(format!("{line}\n").as_bytes()).unwrap();
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    fn read_times(&self) -> Vec<Instant> {
        self.read_times.lock().unwrap().clone()
    }
}

fn start_with(env: &Env) -> ferryd::Daemon {
    let mut o = options(env);
    o.monitors = MonitorSource::HyprlandIn(env.run.clone());
    // the identity lives under the temp HOME, so a second start is the same peer (a layout is
    // keyed by fingerprint)
    o.identity = None;
    ferryd::Daemon::start(o).unwrap()
}

fn monitor_names(c: &mut Client) -> Vec<String> {
    let r = c.result("layout.get", json!(null));
    r["peers"][0]["monitors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn reads_the_monitors_at_start_and_rereads_100ms_after_the_last_event() {
    let env = Env::new();
    let fake = FakeHyprland::start(&env.run, &[("HDMI-A-1", 1920, 1080, 0)]);
    let d = start_with(&env);
    let mut c = Client::connect(d.socket_path());
    assert_eq!(monitor_names(&mut c), ["HDMI-A-1"]);

    // the watcher subscribes and re-reads once on connect (the debounce starts at zero)
    wait_for("the subscription", || {
        fake.events.lock().unwrap().is_some().then_some(())
    });
    wait_for("the first read", || (fake.reads() >= 2).then_some(()));
    let base = fake.reads();

    // A burst of events: one re-read, 100 ms after the last of them. The test thread can be
    // descheduled for longer than that debounce, so what it asserts does not depend on how long
    // its own sleeps took: it times the events as it writes them, and asserts the exact counts
    // only when the burst really was dense.
    fake.set_monitors(&[("HDMI-A-1", 1920, 1080, 0), ("DVI-D-1", 1920, 1080, 1920)]);
    let mut written: Vec<(Instant, Instant)> = Vec::new();
    let mut emit = |line: &str| {
        let before = Instant::now(); // the daemon cannot see the event earlier than this
        fake.emit(line);
        written.push((before, Instant::now()));
    };
    emit("monitoraddedv2>>1,DVI-D-1,desc");
    std::thread::sleep(Duration::from_millis(30));
    emit("configreloaded>>");
    std::thread::sleep(Duration::from_millis(30));
    emit("monitoradded>>DVI-D-1");
    let last = written.last().unwrap().0;
    // Dense: the daemon cannot have seen a gap near the debounce between two events, with room
    // left for its own jitter. The gap between two events is at most from the first one's
    // `before` to the second one's `after`.
    let dense = written
        .windows(2)
        .all(|w| w[1].1 - w[0].0 < Duration::from_millis(60));

    std::thread::sleep(Duration::from_millis(60));
    let early = fake.reads();
    if dense && last.elapsed() < Duration::from_millis(95) {
        // `early` was read before `elapsed()`, so it is from under 95 ms after the last event
        assert_eq!(early, base, "no re-read before the debounce has passed");
    }
    // The read for the last event cannot come earlier than the debounce after it. (An earlier
    // read can be the debounce of an earlier event running out, if the machine stalled mid-burst.)
    let due = last + Duration::from_millis(95);
    let at = wait_for("the re-read after the last event", || {
        fake.read_times().into_iter().find(|t| *t >= due)
    });
    // A sentinel (G-26): the watcher takes events in order, so the read for a later event comes
    // after any read the burst caused. Wait for it, then count.
    let sentinel_due = Instant::now() + Duration::from_millis(95);
    fake.emit("configreloaded>>");
    wait_for("the sentinel's read", || {
        fake.read_times().into_iter().find(|t| *t >= sentinel_due)
    });
    if dense {
        assert_eq!(
            fake.reads(),
            base + 2,
            "the burst caused one read, the sentinel one more"
        );
        assert!(
            at - last < Duration::from_millis(1500),
            "re-read took {:?}",
            at - last
        );
    }

    wait_for("the new monitor", || {
        (monitor_names(&mut c).len() == 2).then_some(())
    });
    assert_eq!(monitor_names(&mut c), ["HDMI-A-1", "DVI-D-1"]);

    // events that do not touch the layout read nothing: a monitor removed after them is the
    // sentinel (G-26), and the read it causes is the only one
    let before = fake.reads();
    fake.emit("workspace>>3");
    fake.emit("focusedmon>>DVI-D-1,3");
    // Pacing, not an assertion: longer than the 100 ms debounce, so a read these events wrongly
    // caused cannot merge with the removal's.
    std::thread::sleep(Duration::from_millis(150));
    fake.set_monitors(&[("DVI-D-1", 1920, 1080, 0)]);
    fake.emit("monitorremoved>>HDMI-A-1");
    wait_for("the removal", || {
        (monitor_names(&mut c) == ["DVI-D-1"]).then_some(())
    });
    assert_eq!(
        fake.reads(),
        before + 1,
        "only the removal read the monitors"
    );
    d.shutdown();
}

/// A daemon on the fake Hyprland with the peer `lap` connected and placed right of DVI-D-1, the
/// way J10 starts. The peer's frames are drained up to the `Layout` the placement pushed.
struct Hotplug {
    env: Env,
    fake: FakeHyprland,
    rt: tokio::runtime::Runtime,
    d: Option<ferryd::Daemon>,
    c: Client,
    peer: FakePeer,
    /// The monitors the peer last heard of, from the daemon's `Hello` or a `Monitors` frame.
    heard: Vec<ferry_core::geom::MonitorInfo>,
}

impl Hotplug {
    fn new() -> Self {
        let env = Env::new();
        let fake = FakeHyprland::start(
            &env.run,
            &[("HDMI-A-1", 1920, 1080, 0), ("DVI-D-1", 1920, 1080, 1920)],
        );
        let d = start_with(&env);
        let mut c = Client::connect(d.socket_path());
        let id = Identity::ephemeral().unwrap();
        c.result(
            "trust",
            json!({"fingerprint": id.fingerprint().to_string(), "name": "lap"}),
        );
        let rt = rt();
        let mut peer = rt
            .block_on(FakePeer::dial(id, d.net_addr(), d.fingerprint()))
            .unwrap();
        let Some(Control::Hello {
            monitors: heard, ..
        }) = rt.block_on(peer.exchange_hello(&hello_now("lap", vec![monitor("eDP-1", 1366, 768)])))
        else {
            panic!("the daemon answers Hello");
        };
        wait_for("lap up", || {
            let st = c.result("status", json!(null));
            (st["peers"][0]["state"] == "connected").then_some(())
        });
        c.result(
            "layout.place",
            json!({"peer": "lap", "rel": "right_of", "anchor": "DVI-D-1"}),
        );
        wait_for("the subscription", || {
            fake.events.lock().unwrap().is_some().then_some(())
        });
        let mut me = Self {
            env,
            fake,
            rt,
            d: Some(d),
            c,
            peer,
            heard,
        };
        // The subscription's first reading may reach the peer before the placement's Layout
        // (a release build is fast enough to see it): skip that Monitors frame.
        let first = loop {
            match me.next_frame() {
                Control::Monitors { monitors } => me.heard = monitors,
                other => break other,
            }
        };
        assert!(matches!(first, Control::Layout { .. }), "{first:?}");
        me
    }

    fn next_frame(&mut self) -> Control {
        self.rt
            .block_on(self.peer.next_frame(Duration::from_secs(5)))
            .expect("a frame")
    }

    /// The monitors of the next `Monitors` frame that changes what the peer heard, and how long it
    /// took from `since`. ferryd sends every reading, so the subscription's first one repeats the
    /// `Hello`'s monitors, and in a release build it can land after the placement's `Layout`.
    fn next_monitors(&mut self, since: Instant) -> (Vec<ferry_core::geom::MonitorInfo>, Duration) {
        loop {
            if let Control::Monitors { monitors } = self.next_frame()
                && monitors != self.heard
            {
                self.heard.clone_from(&monitors);
                return (monitors, since.elapsed());
            }
        }
    }

    /// Hyprland now reports `list`; `event` is what it says about it on socket2.
    fn hyprland(&self, list: &[(&str, u32, u32, i32)], event: &str) -> Instant {
        self.fake.set_monitors(list);
        let at = Instant::now();
        self.fake.emit(event);
        at
    }

    fn layout(&mut self) -> Value {
        self.c.result("layout.get", json!(null))
    }

    /// The plane x of the desk monitor `name`, once `layout.get` lists `count` of them.
    fn desk_x(&mut self, name: &str, count: usize) -> i64 {
        let v = wait_for("the desk monitors", || {
            let v = self.layout();
            (v["peers"][0]["monitors"].as_array()?.len() == count).then_some(v)
        });
        v["peers"][0]["monitors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == name)
            .unwrap()["x"]
            .as_i64()
            .unwrap()
    }
}

/// FR-LAYOUT-03, FR-LAYOUT-11, J10 steps 1 to 3, ADR 0002. Hyprland moves DVI-D-1 to x = 0 when
/// HDMI-A-1 goes: the group stays where it was on the plane, the peer hears of the new monitors
/// at once (with the anchor shift), and no revision is made.
#[test]
fn an_unplugged_monitor_that_hyprland_shifts_leaves_the_group_in_place() {
    let mut h = Hotplug::new();
    let before = h.layout();
    assert_eq!(
        before["peers"][1]["origin"],
        json!([3840, 0]),
        "lap right of DVI-D-1"
    );
    assert_eq!(h.desk_x("DVI-D-1", 2), 1920);

    // J10 step 1: HDMI-A-1 is unplugged and Hyprland moves DVI-D-1 to 0
    let at = h.hyprland(&[("DVI-D-1", 1920, 1080, 0)], "monitorremoved>>HDMI-A-1");
    let (monitors, took) = h.next_monitors(at);
    // The peer learns within a second. It takes the 100 ms debounce, plus a few
    // milliseconds (about 102 ms here, with the machine loaded), so the bound has room.
    assert!(took < Duration::from_secs(1), "{took:?}");
    assert_eq!(monitors.len(), 1);
    assert_eq!(
        (monitors[0].name.as_str(), monitors[0].rect.x),
        ("DVI-D-1", 0)
    );
    assert_eq!(
        monitors[0].shift,
        Some(WireOffset { x: 1920, y: 0 }),
        "it says it was moved"
    );

    // the group did not move on the plane, and nothing was edited
    assert_eq!(h.desk_x("DVI-D-1", 1), 1920);
    let after = h.layout();
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(after["peers"][0]["origin"], before["peers"][0]["origin"]);
    assert_eq!(after["peers"][0]["shift"], json!([1920, 0]));
    assert_eq!(after["peers"][1]["origin"], before["peers"][1]["origin"]);
    // lap still touches DVI-D-1: the link survived the hot-plug
    let links = after["links"].as_array().unwrap();
    assert!(
        links
            .iter()
            .any(|l| l["from"] == "desk:DVI-D-1" && l["to"] == "lap:eDP-1" && l["side"] == "right"),
        "{links:?}"
    );

    // `layout place` knows where the shifted group is: lap right of DVI-D-1 is where it is now.
    // (The placement is an edit: it makes the next revision, and pushes it.)
    let placed = h.c.result(
        "layout.place",
        json!({"peer": "lap", "rel": "right_of", "anchor": "DVI-D-1"}),
    );
    assert_eq!(placed["peers"][1]["origin"], json!([3840, 0]));
    let edited = before["revision"]["counter"].as_u64().unwrap() + 1;
    assert_eq!(placed["revision"]["counter"], edited);

    // J10 step 3: HDMI-A-1 comes back and Hyprland restores 0 and 1920: it is where it was
    let at = h.hyprland(
        &[("HDMI-A-1", 1920, 1080, 0), ("DVI-D-1", 1920, 1080, 1920)],
        "monitoradded>>HDMI-A-1",
    );
    let (monitors, _) = h.next_monitors(at);
    assert_eq!(monitors.len(), 2);
    assert!(monitors.iter().all(|m| m.shift.is_none()), "{monitors:?}");
    assert_eq!(h.desk_x("HDMI-A-1", 2), 0);
    assert_eq!(h.desk_x("DVI-D-1", 2), 1920);
    let back = h.layout();
    assert_eq!(
        back["revision"]["counter"], edited,
        "the replug made no revision"
    );
    assert!(back["peers"][0].get("shift").is_none());
    assert_eq!(back["peers"][1]["origin"], json!([3840, 0]));
    h.d.take().unwrap().shutdown();
}

/// The memory is on disk: a ferryd that starts while HDMI-A-1 is still gone keeps DVI-D-1 in
/// place, and HDMI-A-1 finds its place when it returns ("Unplug and replug").
#[test]
fn the_monitor_memory_survives_a_restart() {
    let mut h = Hotplug::new();
    let at = h.hyprland(&[("DVI-D-1", 1920, 1080, 0)], "monitorremoved>>HDMI-A-1");
    h.next_monitors(at);
    assert_eq!(h.desk_x("DVI-D-1", 1), 1920);
    h.d.take().unwrap().shutdown();
    let file = std::fs::read_to_string(h.env.paths().layout_file()).unwrap();
    assert!(
        file.contains("[anchor]") && file.contains("x = 1920"),
        "{file}"
    );

    // the same Hyprland (DVI-D-1 at 0), a new ferryd
    let d = start_with(&h.env);
    h.c = Client::connect(d.socket_path());
    assert_eq!(
        h.desk_x("DVI-D-1", 1),
        1920,
        "kept in place across the restart"
    );
    assert_eq!(h.layout()["peers"][0]["shift"], json!([1920, 0]));

    // and HDMI-A-1 comes back to its remembered place
    wait_for("the subscription", || {
        h.fake.events.lock().unwrap().is_some().then_some(())
    });
    h.hyprland(
        &[("HDMI-A-1", 1920, 1080, 0), ("DVI-D-1", 1920, 1080, 1920)],
        "monitoradded>>HDMI-A-1",
    );
    assert_eq!(h.desk_x("HDMI-A-1", 2), 0);
    assert_eq!(h.desk_x("DVI-D-1", 2), 1920);
    d.shutdown();
}

#[test]
fn a_missing_compositor_is_not_fatal() {
    let env = Env::new();
    let mut o = options(&env);
    o.monitors = MonitorSource::HyprlandIn(env.run.clone());
    let d = ferryd::Daemon::start(o).unwrap();
    let mut c = Client::connect(d.socket_path());
    assert!(monitor_names(&mut c).is_empty());
    d.shutdown();
}

/// `ferry release` reaches Hyprland's IPC release at once while the portal
/// driver is stuck in a call that never returns. The IPC release used to wait in the driver's
/// queue behind that call, so the socket's documented escape did nothing while capture could
/// be on. The capture side runs against the fake portal; IPC goes to the fake Hyprland.
#[test]
fn ferry_release_reaches_the_ipc_release_while_the_portal_hangs() {
    let env = Env::new();
    let fake = FakeHyprland::start(&env.run, &[("HDMI-A-1", 1920, 1080, 0)]);
    let portal = ferryd::FakePortal::new();
    let mut o = options(&env);
    o.monitors = MonitorSource::HyprlandIn(env.run.clone());
    // `capture` only turns Hyprland IPC on, to the fake under the temp runtime dir; the portal
    // is the fake one. Nothing reaches the live session.
    o.capture = true;
    o.fake_portal = Some(portal.clone());
    let d = ferryd::Daemon::start(o).unwrap();
    let mut c = Client::connect(d.socket_path());
    // the capture side asks for a lock check every 2 s: the next one hangs the driver
    portal.hang();
    wait_for("the driver hung", || portal.hung().then_some(()));

    let asked = Instant::now();
    assert_eq!(c.result("release", json!(null)), json!({}));
    let reached = wait_for("the IPC release", || {
        fake.commands
            .lock()
            .unwrap()
            .iter()
            .find(|(t, c)| *t >= asked && c.contains("release_input_capture"))
            .map(|(t, _)| *t)
    });
    let after = reached - asked;
    assert!(
        after < Duration::from_millis(100),
        "IPC release after {after:?}"
    );
    d.shutdown();
}
