//! Lab drills for `ferry-inject` (M1). **Run only inside a lab VM**: it injects
//! input and creates virtual devices. Every drill refuses (exit 2) before it opens a
//! device unless DMI says QEMU/KVM or `FERRY_LAB=1` is set; `flood` is
//! stricter and takes only DMI, because it stops the compositor.
//!
//! ```text
//! drill hold [CODE...]   start the injector, press the evdev CODEs, print READY,
//!                        then wait to be killed (S10's kill -9 drill)
//! drill aim [--no-follow]  layout re-read (--no-follow is the control: the layout
//!                        is set once at start and never re-read): aim at the corner pixels of every monitor,
//!                        add and remove an output, change mode and scale, aim again
//! drill keymap           switch us / us-intl four times, typing the same keys
//! drill flood [N]        stop the compositor, fill the socket, queue N more keys
//!                        (N <= 200 stay in the ring, more overflow), resume it
//! ```
//!
//! Every drill prints one line per check and ends with `RESULT ok` or
//! `RESULT FAIL`; the exit code follows.
#![expect(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a lab tool: its output is the product"
)]

use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use ferry_hypr_ipc::Instance;
use ferry_inject::{
    Input, InputConfig, InputEvent, KeymapNames, Layout, PointerMode, Target, keymap_from_names,
};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    if let Err(why) = refuse_outside_lab() {
        eprintln!("{why}");
        return ExitCode::from(2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("hold") => hold(&args[1..]),
        Some("aim") => aim(args.get(1).is_some_and(|a| a == "--no-follow")),
        Some("keymap") => keymap(),
        Some("flood") => flood(&args[1..]),
        _ => {
            eprintln!("usage: drill hold [CODE...] | aim | keymap | flood [N]");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => {
            println!("RESULT ok");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("RESULT FAIL");
            ExitCode::FAILURE
        }
        Err(e) => {
            println!("RESULT FAIL {e}");
            ExitCode::FAILURE
        }
    }
}

fn start(
    layout: Layout,
    follow: bool,
    keymap: Option<String>,
) -> Res<(Input, std::sync::mpsc::Receiver<String>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let input = Input::start(
        InputConfig {
            target: Target::default(),
            pointer: PointerMode::Unbound,
            layout,
            keymap,
        },
        Box::new(move |e| {
            let line = match e {
                InputEvent::Ready => "READY_EVENT".to_owned(),
                other => format!("{other:?}"),
            };
            let _ = tx.send(line);
        }),
    )?;
    if follow {
        input.follow_layout(&Target::default())?;
    }
    Ok((input, rx))
}

/// Start with the keyboard reset, press the codes, wait to be killed.
fn hold(codes: &[String]) -> Res<bool> {
    let (input, _events) = start(Layout::default(), false, None)?;
    println!("STARTED pid={}", std::process::id());
    for c in codes {
        input.key(c.parse()?, true)?;
    }
    input.sync()?;
    println!("READY held={codes:?}");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

// ----- aim ---------------------------------------------------------------

struct Rect {
    name: String,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}

fn rects(layout: &Layout) -> Vec<Rect> {
    layout
        .outputs
        .iter()
        .map(|o| Rect {
            name: o.name.clone(),
            x: o.x as i64,
            y: o.y as i64,
            w: o.w as i64,
            h: o.h as i64,
        })
        .collect()
}

/// Aim at the four corner pixels of every monitor (and the two pixels either
/// side of a seam) and read each position back with Hyprland's `cursorpos`.
fn aim_corners(input: &Input, ipc: &Instance, label: &str) -> Res<(u32, u32)> {
    let layout = Layout::from_monitors(&ipc.monitors()?);
    let mut points: Vec<(String, i64, i64)> = Vec::new();
    for r in rects(&layout) {
        for (dx, dy, what) in [
            (0, 0, "top-left"),
            (r.w - 1, 0, "top-right"),
            (0, r.h - 1, "bottom-left"),
            (r.w - 1, r.h - 1, "bottom-right"),
        ] {
            points.push((format!("{}:{what}", r.name), r.x + dx, r.y + dy));
        }
    }
    let (mut ok, mut miss) = (0, 0);
    for (what, x, y) in points {
        // Aim from elsewhere first, so a repeated target is a real motion.
        input.motion((x + 7) as f64, (y + 5) as f64)?;
        input.motion(x as f64, y as f64)?;
        input.sync()?;
        let p = ipc.cursor_pos()?;
        let (gx, gy) = (p.x.floor() as i64, p.y.floor() as i64);
        let hit = (gx, gy) == (x, y);
        println!(
            "AIM {label} {what} want=({x},{y}) got=({},{}) {}",
            p.x,
            p.y,
            if hit { "ok" } else { "MISS" }
        );
        if hit {
            ok += 1;
        } else {
            miss += 1;
        }
    }
    Ok((ok, miss))
}

fn wait_layout_updates(input: &Input, at_least: u64, within: Duration) -> Option<Duration> {
    let t0 = Instant::now();
    while t0.elapsed() < within {
        if input.stats().layout_updates >= at_least {
            return Some(t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn aim(control: bool) -> Res<bool> {
    let ipc = Instance::discover(None)?;
    let follow = !control;
    let (input, _events) = start(Layout::default(), follow, None)?;
    if control {
        input.set_layout(Layout::from_monitors(&ipc.monitors()?))?;
        input.sync()?;
    }
    let mut good = true;
    let mut total = (0u32, 0u32);
    let mut add = |(ok, miss): (u32, u32)| {
        total.0 += ok;
        total.1 += miss;
    };
    let mut check = |what: &str, got: Option<Duration>| match got {
        Some(d) => println!(
            "LAYOUT {what}: re-read {} ms after the change",
            d.as_millis()
        ),
        None => {
            println!("LAYOUT {what}: NOT re-read within 2 s");
            good = false;
        }
    };
    let mut updates = input.stats().layout_updates;
    println!("LAYOUT start: {updates} update(s)");
    add(aim_corners(&input, &ipc, "base")?);

    // 1. add an output (socket2 monitoradded).
    let before: Vec<String> = ipc.monitors()?.into_iter().map(|m| m.name).collect();
    ipc.dispatch("output create headless")?;
    let t0 = Instant::now();
    let added = loop {
        let now: Vec<String> = ipc.monitors()?.into_iter().map(|m| m.name).collect();
        if let Some(n) = now.into_iter().find(|n| !before.contains(n)) {
            break n;
        }
        if t0.elapsed() > Duration::from_secs(5) {
            return Err("no output appeared".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    println!("OUTPUT added {added}");
    updates += 1;
    check(
        "add",
        wait_layout_updates(&input, updates, Duration::from_secs(2)),
    );
    add(aim_corners(&input, &ipc, "added")?);

    // 2. mode and scale of the first monitor (a wl_output change; no socket2 event).
    let first = ipc.monitors()?.remove(0);
    ipc.set_monitor_mode(&first.name, 1600, 900, 60, 1.25)?;
    updates += 1;
    check(
        "mode+scale",
        wait_layout_updates(&input, updates, Duration::from_secs(2)),
    );
    add(aim_corners(&input, &ipc, "scaled")?);
    ipc.set_monitor_mode(&first.name, 1920, 1080, 60, 1.0)?;
    updates += 1;
    check(
        "mode+scale back",
        wait_layout_updates(&input, updates, Duration::from_secs(2)),
    );
    add(aim_corners(&input, &ipc, "restored")?);

    // 3. remove the output (socket2 monitorremoved).
    ipc.remove_output(&added)?;
    println!("OUTPUT removed {added}");
    updates += 1;
    check(
        "remove",
        wait_layout_updates(&input, updates, Duration::from_secs(2)),
    );
    add(aim_corners(&input, &ipc, "removed")?);

    println!(
        "AIM total points={} ok={} miss={}",
        total.0 + total.1,
        total.0,
        total.1
    );
    println!("STATS {:?}", input.stats());
    if control {
        println!(
            "CONTROL without re-reading, {} of {} points missed",
            total.1,
            total.0 + total.1
        );
        return Ok(total.1 > 0);
    }
    Ok(good && total.1 == 0)
}

// ----- keymap ------------------------------------------------------------

fn names(variant: &str) -> Res<String> {
    Ok(keymap_from_names(&KeymapNames {
        layout: "us".into(),
        variant: variant.into(),
        ..Default::default()
    })?)
}

/// `'a`, then Shift+`'` and `e`, then space: `'a "e ` on us, `á ë ` on us-intl.
fn type_segment(input: &Input) -> Res<()> {
    const APOSTROPHE: u32 = 40;
    const A: u32 = 30;
    const E: u32 = 18;
    const SPACE: u32 = 57;
    const SHIFT: u32 = 42;
    for (code, pressed) in [
        (APOSTROPHE, true),
        (APOSTROPHE, false),
        (A, true),
        (A, false),
        (SPACE, true),
        (SPACE, false),
        (SHIFT, true),
        (APOSTROPHE, true),
        (APOSTROPHE, false),
        (SHIFT, false),
        (E, true),
        (E, false),
        (SPACE, true),
        (SPACE, false),
    ] {
        input.key(code, pressed)?;
    }
    input.sync()?;
    std::thread::sleep(Duration::from_millis(60));
    Ok(())
}

fn keymap() -> Res<bool> {
    let us = names("")?;
    let intl = names("intl")?;
    // Start on intl, so that all four segments below are real keymap changes.
    let (input, _events) = start(Layout::default(), false, Some(intl.clone()))?;
    let mut expected = String::new();
    for (i, (text, out)) in [
        (&us, "'a \"e "),
        (&intl, "á ë "),
        (&us, "'a \"e "),
        (&intl, "á ë "),
    ]
    .into_iter()
    .enumerate()
    {
        input.set_keymap(text.clone())?;
        input.sync()?;
        type_segment(&input)?;
        expected.push_str(out);
        println!(
            "SEGMENT {i} keymap={} keyboards={}",
            if i % 2 == 0 { "us" } else { "us-intl" },
            input.stats().keyboards
        );
    }
    println!("EXPECTED {expected:?}");
    // Same text again must not make another keyboard.
    let before = input.stats().keyboards;
    input.set_keymap(intl.clone())?;
    input.sync()?;
    let after = input.stats().keyboards;
    println!("KEYBOARDS repeat-same-keymap before={before} after={after}");
    println!("STATS {:?}", input.stats());
    Ok(before == after && before == 5)
}

/// These drills create virtual devices and press keys, so on a machine that is
/// not a lab VM they must never start. `FERRY_LAB=1` is the explicit override.
fn refuse_outside_lab() -> Result<(), &'static str> {
    if in_qemu() || std::env::var_os("FERRY_LAB").is_some_and(|v| v == "1") {
        Ok(())
    } else {
        Err(
            "refusing: this drill injects input and is for a lab VM only (DMI is not QEMU/KVM). \
             Set FERRY_LAB=1 to override on a machine you have cleared for it",
        )
    }
}

// ----- flood -------------------------------------------------------------

fn in_qemu() -> bool {
    ["product_name", "sys_vendor"].iter().any(|f| {
        std::fs::read_to_string(format!("/sys/class/dmi/id/{f}"))
            .map(|s| s.contains("QEMU") || s.contains("KVM") || s.contains("Standard PC"))
            .unwrap_or(false)
    })
}

fn signal_compositor(sig: &str) -> Res<()> {
    let st = Command::new("pkill")
        .args([sig, "-x", "Hyprland"])
        .status()?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("pkill {sig} Hyprland: {st}").into())
    }
}

/// Resumes the compositor however the drill ends.
struct Resume;
impl Drop for Resume {
    fn drop(&mut self) {
        let _ = signal_compositor("-CONT");
    }
}

fn flood(args: &[String]) -> Res<bool> {
    if !in_qemu() {
        return Err("refusing to stop a compositor on a machine that is not a QEMU guest".into());
    }
    let extra: u32 = args.first().map_or(Ok(100), |s| s.parse())?;
    let (input, events) = start(Layout::default(), false, None)?;
    // One key held through the freeze, so an overflow has something to release.
    // `x`, not a modifier: Super and Shift would fire Omarchy's binds.
    input.key(45, true)?;
    input.sync()?;
    println!("HELD x");
    let guard = Resume;
    signal_compositor("-STOP")?;
    std::thread::sleep(Duration::from_millis(300));
    // Fill the socket. Letters a..z; the wev log gets them as presses/releases.
    let mut sent = 0u64;
    let t0 = Instant::now();
    while input.stats().would_block == 0 {
        let code = 16 + (sent / 2 % 10) as u32; // q..p
        input.key(code, sent.is_multiple_of(2))?;
        sent += 1;
        if sent.is_multiple_of(64) {
            // Let the inject thread keep up, so the channel never fills first.
            std::thread::sleep(Duration::from_micros(300));
        }
        if t0.elapsed() > Duration::from_secs(20) {
            return Err("the socket never blocked".into());
        }
    }
    if !sent.is_multiple_of(2) {
        // Finish the press/release pair, so only `x` stays held.
        input.key(16 + (sent / 2 % 10) as u32, false)?;
        sent += 1;
    }
    std::thread::sleep(Duration::from_millis(200));
    println!("BLOCKED after {sent} key events, stats {:?}", input.stats());
    // The marker run: `extra` events more, every one distinct in order.
    for i in 0..extra {
        input.key(30 + (i / 2 % 9), i % 2 == 0)?; // a, s, d, ... (KEY_A.. row)
        std::thread::sleep(Duration::from_micros(100));
    }
    std::thread::sleep(Duration::from_millis(300));
    println!("QUEUED {extra} more, stats {:?}", input.stats());
    drop(guard);
    input.sync()?;
    std::thread::sleep(Duration::from_millis(500));
    let mut overflow_events = 0;
    while let Ok(line) = events.try_recv() {
        if line.starts_with("Overflow") {
            overflow_events += 1;
        }
        println!("EVENT {line}");
    }
    let stats = input.stats();
    println!(
        "DONE sent_to_fill={sent} marker_events={extra} overflow_events={overflow_events} stats={stats:?}"
    );
    let want_overflow = extra as usize > 200;
    let ok = if want_overflow {
        overflow_events >= 1 && stats.overflows >= 1
    } else {
        overflow_events == 0 && stats.overflows == 0
    };
    // Releases are the point: ask the injector what it still holds.
    input.release_all()?;
    input.sync()?;
    Ok(ok)
}
