//! fake-peer: drive a ferryd over the network as a fake source or a fake target. **Network
//! only**: it never captures or injects, so it may run on the host against a lab VM.
//!
//! ```text
//! fake-peer id <key-dir>                                   print this fake's fingerprint
//! fake-peer source <key-dir> <ip:port> <daemon-fp> [--name N] [--monitor WxH] [--layout us | --keymap-file F]
//! fake-peer target <key-dir> <ip:port> <daemon-fp> [--name N] [--monitor WxH]
//! ```
//!
//! Both roles **dial** the daemon (the lab's host firewall drops guest → host UDP), send
//! `Hello`, and Ping every 2 s. Every line printed is `<mono_us> <TAG> ...`, host
//! `CLOCK_MONOTONIC`.
//!
//! **source** sends the keymap at once, then reads commands from stdin, one per line:
//!
//! | Command | Does |
//! |---|---|
//! | `enter X Y` | `Enter` at target-local (X, Y), then `Held#1`; `Held` every 100 ms from then |
//! | `move X Y` | one motion datagram |
//! | `glide X0 Y0 X1 Y1 STEPS MS` | STEPS motion datagrams, MS apart |
//! | `key CODE down\|up` · `tap CODE...` · `chord CODE...` | evdev key edges |
//! | `type TEXT` | ASCII text as US key taps (Shift for upper case and symbols) |
//! | `button CODE down\|up` · `click left\|right\|middle` | buttons (with the position fence) |
//! | `wheel N` · `hwheel N` | N notches (value120 = 120·N), + is down / right |
//! | `leave` | `Leave(home)` |
//! | `keymap FILE` | a new `Keymap` (xkb text from FILE) mid-session, as a source whose keymap changes |
//! | `mute` · `unmute` | stop / resume the `Held` snapshots (the target's watchdog then fires) |
//! | `sleep MS` · `quit` | |
//!
//! **target** answers `Hello`, acknowledges every `Held` (unless `mute`), and prints
//! everything: `Enter`, keys, buttons, scroll, `Held` with the gap since the previous one,
//! `Leave` with its reason, motion datagrams. On `Leave` it prints the time since the last
//! `HeldAck` it sent. stdin: `mute`, `unmute`, `stats`, `quit`.
#![expect(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a lab tool: its output is the product"
)]

use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferry_core::wire::{Control, Datagram, LeaveReason};
use ferry_net::{Fingerprint, Identity};
use ferryd::fake_peer::{FakePeer, Incoming, Sender, decode, hello_now, monitor};
use tokio::io::AsyncBufReadExt;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn mono_us() -> u64 {
    ferryd::clock::now().0
}

macro_rules! out {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{} {}", mono_us(), line);
        let _ = o.flush();
    }};
}

struct Args {
    dir: String,
    addr: SocketAddr,
    fp: Fingerprint,
    name: String,
    monitor: (u32, u32),
    layout: String,
    keymap_file: Option<String>,
}

fn parse(rest: &[String]) -> Res<Args> {
    let [dir, addr, fp, opts @ ..] = rest else {
        return Err("need <key-dir> <ip:port> <daemon-fp>".into());
    };
    let mut a = Args {
        dir: dir.clone(),
        addr: addr.parse()?,
        fp: fp.parse().map_err(|e| format!("fingerprint: {e:?}"))?,
        name: "fake".into(),
        monitor: (1366, 768),
        layout: "us".into(),
        keymap_file: None,
    };
    let mut it = opts.iter();
    while let Some(o) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{o} needs a value"));
        match o.as_str() {
            "--name" => a.name = val()?,
            "--monitor" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--monitor WxH")?;
                a.monitor = (w.parse()?, h.parse()?);
            }
            "--layout" => a.layout = val()?,
            "--keymap-file" => a.keymap_file = Some(val()?),
            other => return Err(format!("unknown option {other}").into()),
        }
    }
    Ok(a)
}

async fn connect(a: &Args) -> Res<FakePeer> {
    let id = Identity::load_or_create(std::path::Path::new(&a.dir))?;
    let mut p = FakePeer::dial(id, a.addr, a.fp).await?;
    let ours = hello_now(&a.name, vec![monitor("eDP-1", a.monitor.0, a.monitor.1)]);
    match p.exchange_hello(&ours).await {
        Some(Control::Hello {
            proto_major,
            proto_minor,
            name,
            monitors,
            ..
        }) => {
            let m: Vec<String> = monitors
                .iter()
                .map(|m| {
                    format!(
                        "{}@{},{}:{}x{}",
                        m.name, m.rect.x, m.rect.y, m.rect.w, m.rect.h
                    )
                })
                .collect();
            out!(
                "HELLO name={name} proto={proto_major}.{proto_minor} monitors={}",
                m.join(",")
            );
        }
        other => return Err(format!("no Hello from the daemon: {other:?}").into()),
    }
    Ok(p)
}

/// Ping every 2 s, as a dialer does (it keeps the peer from staying suspect).
fn spawn_pinger(conn: ferry_net::Connection) {
    tokio::spawn(async move {
        let mut id = 0u32;
        let mut t = tokio::time::interval(Duration::from_secs(2));
        loop {
            t.tick().await;
            id = id.wrapping_add(1);
            let mut b = [0u8; ferry_core::wire::DATAGRAM_LEN];
            Datagram::Ping {
                id,
                t_us: ferryd::clock::now().low32(),
            }
            .encode(&mut b);
            if conn
                .send_datagram(bytes::Bytes::copy_from_slice(&b))
                .is_err()
            {
                return;
            }
        }
    });
}

// --- source ------------------------------------------------------------------------------------

struct Src {
    tx: Sender,
    keys: Vec<u16>,
    buttons: Vec<u16>,
    entered: bool,
    muted: bool,
}

type Shared = Arc<Mutex<Src>>;

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, Src> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// US positions: (evdev code, shift).
fn us_key(c: char) -> Option<(u16, bool)> {
    const ROW: &[(&str, u16)] = &[
        ("1234567890", 2),
        ("qwertyuiop", 16),
        ("asdfghjkl", 30),
        ("zxcvbnm", 44),
    ];
    const SHIFTED: &str = "!@#$%^&*()";
    if let Some(i) = SHIFTED.find(c) {
        return Some((2 + i as u16, true));
    }
    let lower = c.to_ascii_lowercase();
    for (row, start) in ROW {
        if let Some(i) = row.find(lower) {
            return Some((start + i as u16, c.is_ascii_uppercase()));
        }
    }
    Some(match c {
        ' ' => (57, false),
        '\n' => (28, false),
        '-' => (12, false),
        '_' => (12, true),
        '=' => (13, false),
        '+' => (13, true),
        '.' => (52, false),
        ',' => (51, false),
        '/' => (53, false),
        '?' => (53, true),
        ':' => (39, true),
        ';' => (39, false),
        _ => return None,
    })
}

fn button_code(name: &str) -> Option<u16> {
    Some(match name {
        "left" => 0x110,
        "right" => 0x111,
        "middle" => 0x112,
        n => n.parse().ok()?,
    })
}

fn edge(s: &mut Src, code: u16, pressed: bool) {
    let set = if (0x110..0x120).contains(&code) {
        &mut s.buttons
    } else {
        &mut s.keys
    };
    if pressed {
        if !set.contains(&code) {
            set.push(code);
        }
    } else {
        set.retain(|k| *k != code);
    }
    let r = if (0x110..0x120).contains(&code) {
        s.tx.button(code, pressed)
    } else {
        s.tx.key(code, pressed)
    };
    if let Err(e) = r {
        out!("ERR send {e}");
    }
}

async fn run_source(a: Args) -> Res<()> {
    let p = connect(&a).await?;
    let (_ep, mut tx, mut events) = p.into_parts();
    spawn_pinger(tx.conn.clone());
    let names = ferry_inject::KeymapNames {
        rules: "evdev".into(),
        model: "pc105".into(),
        layout: a.layout.clone(),
        variant: String::new(),
        options: None,
    };
    let keymap = match &a.keymap_file {
        Some(path) => std::fs::read_to_string(path)?
            .trim_end_matches('\0')
            .to_string(),
        None => ferry_inject::keymap_from_names(&names)?,
    };
    tx.keymap(&keymap)?;
    out!("KEYMAP serial={} len={}", tx.keymap_serial, keymap.len());
    let s: Shared = Arc::new(Mutex::new(Src {
        tx,
        keys: Vec::new(),
        buttons: Vec::new(),
        entered: false,
        muted: false,
    }));

    // Held every 100 ms while entered.
    let held = s.clone();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(Duration::from_millis(100));
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            let mut g = lock(&held);
            if g.entered && !g.muted {
                let (k, b) = (g.keys.clone(), g.buttons.clone());
                let _ = g.tx.held(&k, &b);
            }
        }
    });

    // What the daemon says.
    tokio::spawn(async move {
        let mut acks = 0u64;
        while let Some(ev) = events.next().await {
            match decode(ev) {
                Incoming::Frame(Control::HeldAck { session, n }) => {
                    acks += 1;
                    if n <= 2 || acks.is_multiple_of(50) {
                        out!("ACK session={session} n={n} total={acks}");
                    }
                }
                Incoming::Frame(Control::Ended { session, reason }) => {
                    out!(
                        "ENDED session={session} reason={}",
                        LeaveReason::from_code(reason).as_str()
                    );
                }
                Incoming::Frame(m) => out!("FRAME {}", short(&m)),
                Incoming::Datagram(Datagram::Pong { id, t_us }) => {
                    let rtt = ferryd::clock::now().low32().wrapping_sub(t_us);
                    if id % 15 == 1 {
                        out!("PONG id={id} rtt_us={rtt}");
                    }
                }
                Incoming::Closed(r) => {
                    out!("CLOSED {r}");
                    std::process::exit(0);
                }
                other => out!("IN {other:?}"),
            }
        }
    });

    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let w: Vec<&str> = line.split_whitespace().collect();
        let Some((cmd, args)) = w.split_first() else {
            continue;
        };
        let num =
            |i: usize| -> Res<f64> { Ok(args.get(i).ok_or("missing argument")?.parse::<f64>()?) };
        let r: Res<()> = async {
            match *cmd {
                "enter" => {
                    let mut g = lock(&s);
                    g.keys.clear();
                    g.buttons.clear();
                    g.tx.enter(num(0)? as f32, num(1)? as f32)?;
                    g.entered = true;
                    out!("ENTER session={} x={} y={}", g.tx.session, num(0)?, num(1)?);
                }
                "move" => {
                    lock(&s).tx.motion(num(0)? as f32, num(1)? as f32);
                }
                "glide" => {
                    let (x0, y0, x1, y1) = (num(0)?, num(1)?, num(2)?, num(3)?);
                    let (steps, ms) = (num(4)? as u32, num(5)?);
                    for i in 1..=steps {
                        let f = f64::from(i) / f64::from(steps);
                        lock(&s)
                            .tx
                            .motion((x0 + (x1 - x0) * f) as f32, (y0 + (y1 - y0) * f) as f32);
                        tokio::time::sleep(Duration::from_secs_f64(ms / 1000.0)).await;
                    }
                }
                "key" | "button" => {
                    let code = if *cmd == "button" {
                        button_code(args.first().ok_or("code")?).ok_or("button")?
                    } else {
                        args.first().ok_or("code")?.parse()?
                    };
                    let down = args.get(1) == Some(&"down");
                    edge(&mut lock(&s), code, down);
                }
                "click" => {
                    let code = button_code(args.first().unwrap_or(&"left")).ok_or("button")?;
                    edge(&mut lock(&s), code, true);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    edge(&mut lock(&s), code, false);
                }
                "tap" => {
                    for c in args {
                        let code: u16 = c.parse()?;
                        edge(&mut lock(&s), code, true);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        edge(&mut lock(&s), code, false);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
                "chord" => {
                    let codes: Vec<u16> =
                        args.iter().map(|c| c.parse()).collect::<Result<_, _>>()?;
                    for c in &codes {
                        edge(&mut lock(&s), *c, true);
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                    for c in codes.iter().rev() {
                        edge(&mut lock(&s), *c, false);
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                }
                "type" => {
                    let text = line
                        .split_once(' ')
                        .map_or("", |(_, t)| t)
                        .replace("\\n", "\n");
                    for ch in text.chars() {
                        let Some((code, shift)) = us_key(ch) else {
                            out!("SKIP {ch:?}");
                            continue;
                        };
                        if shift {
                            edge(&mut lock(&s), 42, true);
                        }
                        edge(&mut lock(&s), code, true);
                        tokio::time::sleep(Duration::from_millis(15)).await;
                        edge(&mut lock(&s), code, false);
                        if shift {
                            edge(&mut lock(&s), 42, false);
                        }
                        tokio::time::sleep(Duration::from_millis(15)).await;
                    }
                }
                "wheel" | "hwheel" => {
                    let n = num(0)? as i32;
                    let axis = u8::from(*cmd == "hwheel");
                    let step = n.signum();
                    for _ in 0..n.abs() {
                        lock(&s).tx.wheel(axis, 120 * step)?;
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                }
                "leave" => {
                    let mut g = lock(&s);
                    g.tx.leave(LeaveReason::Home)?;
                    g.entered = false;
                }
                "keymap" => {
                    let path = args.first().ok_or("keymap FILE")?;
                    let text = std::fs::read_to_string(path)?;
                    let mut g = lock(&s);
                    g.tx.keymap(text.trim_end_matches('\0'))?;
                    out!("KEYMAP serial={} len={}", g.tx.keymap_serial, text.len());
                }
                "mute" => {
                    lock(&s).muted = true;
                    out!("MUTED");
                }
                "unmute" => {
                    lock(&s).muted = false;
                    out!("UNMUTED");
                }
                "sleep" => tokio::time::sleep(Duration::from_secs_f64(num(0)? / 1000.0)).await,
                "quit" => std::process::exit(0),
                other => out!("ERR unknown command {other}"),
            }
            Ok(())
        }
        .await;
        match r {
            Ok(()) => out!("OK {line}"),
            Err(e) => out!("ERR {line}: {e}"),
        }
    }
    // stdin closed: keep the session (and its snapshots) running until killed.
    std::future::pending::<()>().await;
    Ok(())
}

fn short(m: &Control) -> String {
    match m {
        Control::Keymap { serial, text, .. } => {
            format!("Keymap serial={serial} len={}", text.len())
        }
        Control::Layout { rev, origins } => {
            format!("Layout counter={} origins={}", rev.counter, origins.len())
        }
        other => format!("{other:?}"),
    }
}

// --- target ------------------------------------------------------------------------------------

async fn run_target(a: Args) -> Res<()> {
    let p = connect(&a).await?;
    let (_ep, tx, mut events) = p.into_parts();
    spawn_pinger(tx.conn.clone());
    let tx = Arc::new(Mutex::new(tx));
    let muted = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let m2 = muted.clone();
    let tx2 = tx.clone();
    tokio::spawn(async move {
        let mut last_held: Option<u64> = None;
        let mut last_ack: Option<u64> = None;
        let mut gaps: Vec<u64> = Vec::new();
        let mut motions = 0u64;
        while let Some(ev) = events.next().await {
            let now = mono_us();
            match decode(ev) {
                Incoming::Frame(Control::Held {
                    session,
                    n,
                    keys,
                    buttons,
                    pos,
                }) => {
                    let gap = last_held.map(|t| now - t);
                    last_held = Some(now);
                    if let Some(g) = gap
                        && n > 1
                    {
                        gaps.push(g);
                    }
                    if !m2.load(std::sync::atomic::Ordering::SeqCst) {
                        let mut t = tx2.lock().unwrap_or_else(|e| e.into_inner());
                        let _ = t.send(&Control::HeldAck { session, n });
                        last_ack = Some(now);
                    }
                    if n <= 2 || !keys.is_empty() || !buttons.is_empty() || n % 20 == 0 {
                        out!(
                            "HELD session={session} n={n} gap_us={} keys={keys:?} buttons={buttons:?} pos=({},{})",
                            gap.unwrap_or(0),
                            pos.x,
                            pos.y
                        );
                    }
                }
                Incoming::Frame(Control::Leave { session, reason }) => {
                    let since_ack = last_ack.map(|t| now - t);
                    out!(
                        "LEAVE session={session} reason={} since_last_ack_us={}",
                        LeaveReason::from_code(reason).as_str(),
                        since_ack.unwrap_or(0)
                    );
                    if !gaps.is_empty() {
                        gaps.sort_unstable();
                        let pct = |p: usize| gaps[(gaps.len() - 1) * p / 100];
                        out!(
                            "HELD_GAPS n={} min_us={} p50_us={} p95_us={} max_us={}",
                            gaps.len(),
                            gaps[0],
                            pct(50),
                            pct(95),
                            gaps[gaps.len() - 1]
                        );
                        gaps.clear();
                    }
                    out!("MOTIONS {motions}");
                    motions = 0;
                    last_held = None;
                }
                Incoming::Frame(Control::Enter {
                    session,
                    x,
                    y,
                    keymap_serial,
                    motion_seq,
                }) => {
                    out!(
                        "ENTER session={session} x={x} y={y} keymap={keymap_serial} motion_seq={motion_seq}"
                    );
                }
                Incoming::Frame(Control::Key {
                    session,
                    code,
                    pressed,
                }) => {
                    out!(
                        "KEY session={session} code={code} {}",
                        if pressed { "down" } else { "up" }
                    );
                }
                Incoming::Frame(m) => out!("FRAME {}", short(&m)),
                Incoming::Datagram(Datagram::Motion(m)) => {
                    motions += 1;
                    out!(
                        "MOTION seq={} x={} y={} dx={} dy={}",
                        m.seq,
                        m.x,
                        m.y,
                        m.dx,
                        m.dy
                    );
                }
                Incoming::Datagram(_) => {}
                Incoming::Closed(r) => {
                    out!("CLOSED {r}");
                    std::process::exit(0);
                }
                other => out!("IN {other:?}"),
            }
        }
    });

    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        match line.trim() {
            "mute" => {
                muted.store(true, std::sync::atomic::Ordering::SeqCst);
                out!("MUTED");
            }
            "unmute" => {
                muted.store(false, std::sync::atomic::Ordering::SeqCst);
                out!("UNMUTED");
            }
            "quit" => std::process::exit(0),
            "" => {}
            other => out!("ERR unknown command {other}"),
        }
    }
    std::future::pending::<()>().await;
    Ok(())
}

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((sub, rest)) = args.split_first() else {
        eprintln!(
            "usage: fake-peer id <dir> | source|target <dir> <ip:port> <fp> [--name N] [--monitor WxH] [--layout us | --keymap-file F]"
        );
        std::process::exit(2);
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    match sub.as_str() {
        "id" => {
            let dir = rest.first().ok_or("need <key-dir>")?;
            let id = Identity::load_or_create(std::path::Path::new(dir))?;
            println!("{}", id.fingerprint());
            Ok(())
        }
        "source" => rt.block_on(run_source(parse(rest)?)),
        "target" => rt.block_on(run_target(parse(rest)?)),
        other => Err(format!("unknown subcommand {other}").into()),
    }
}
