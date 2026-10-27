//! capture-probe: the real portal session plus EIS, without the SourceMachine.
//! **Lab VM only**: it captures input. It refuses (exit 2) before it touches the
//! portal or any device unless DMI says QEMU/KVM or `FERRY_LAB=1` is set.
//!
//! It plays ferryd's two capture-side threads: `main` is `ctl` (tokio: portal,
//! Hyprland IPC, logind) and a second thread is `capture` (calloop: the EIS
//! receiver). One barrier sits on one edge of one zone. Every `SourceEvent` is
//! printed as `<epoch_us> <mono_us> <TAG> ...`, so guest-side `date +%s%6N`
//! marks share its clock.
//!
//! ```text
//! capture-probe run --edge right [--zone 0] [--inset 2] [--bounce-after-ms 0 | --hold]
//!                   [--poll-ms 0] [--ipc-dir DIR] [--no-logind]
//! capture-probe lock [--n 10] [--interval-ms 100] [--ipc-dir DIR]
//! capture-probe release
//! capture-probe observe [interval_us=1000] [seconds=10]
//! ```
//!
//! `run` answers each `Activated` with a lock check and then a `Release` `inset`
//! px inside the edge (a bounce), unless `--hold`. It re-zones 100 ms after the
//! last `ZonesChanged`/`Disabled`, and re-creates the session when it is lost
//! or EIS ends. Signals: SIGUSR1 = ferry's own `Disable`, SIGUSR2 = re-arm
//! (barriers + `Enable`), SIGINT/SIGTERM = release, close, exit.
#![expect(
    clippy::print_stderr,
    reason = "a lab tool: it reports to the operator on stderr"
)]

use std::io::Write;
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use calloop::EventLoop;
use calloop::channel::{Event as ChannelEvent, channel as calloop_channel};
use ferry_capture::{
    EisEnd, EisOutput, EisSource, LOCK_CHECK_TIMEOUT, Logind, Portal, PortalSignal, Zone,
    edge_barrier, inside_point, lock_check, lock_check_async, release_input_capture,
};
use ferry_core::PeerId;
use ferry_core::geom::{Barrier, MonitorId, Side};
use ferry_core::source::{LockCheck, PortalCmd, SourceEvent};
use ferry_hypr_ipc::Instance;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::time::sleep_until;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Sleeps until `at`, and never wakes when there is none (a `select!` arm for an optional timer).
async fn sleep_until_opt(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn epoch_us() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0)
}

/// `CLOCK_MONOTONIC` in µs: the clock EIS stamps events with.
fn mono_us() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1000
}

macro_rules! out {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{} {} {}", epoch_us(), mono_us(), line);
        let _ = o.flush();
    }};
}

#[derive(Debug, Clone)]
struct Cfg {
    side: Side,
    zone: usize,
    inset: f64,
    /// `None` = hold (never bounce).
    bounce_after: Option<Duration>,
    poll: Option<Duration>,
    ipc_dir: Option<String>,
    logind: bool,
}

fn parse_run(args: &[String]) -> Res<Cfg> {
    let mut c = Cfg {
        side: Side::Right,
        zone: 0,
        inset: 2.0,
        bounce_after: Some(Duration::ZERO),
        poll: None,
        ipc_dir: None,
        logind: true,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--edge" => {
                c.side = match val()?.as_str() {
                    "left" => Side::Left,
                    "right" => Side::Right,
                    "top" => Side::Top,
                    "bottom" => Side::Bottom,
                    e => return Err(format!("bad edge {e}").into()),
                }
            }
            "--zone" => c.zone = val()?.parse()?,
            "--inset" => c.inset = val()?.parse()?,
            "--bounce-after-ms" => c.bounce_after = Some(Duration::from_millis(val()?.parse()?)),
            "--hold" => c.bounce_after = None,
            "--poll-ms" => {
                let ms: u64 = val()?.parse()?;
                c.poll = (ms > 0).then(|| Duration::from_millis(ms));
            }
            "--ipc-dir" => c.ipc_dir = Some(val()?),
            "--no-logind" => c.logind = false,
            other => return Err(format!("unknown arg {other}").into()),
        }
    }
    Ok(c)
}

fn ipc_for(dir: Option<&str>) -> Res<Instance> {
    Ok(match dir {
        Some(d) => Instance {
            signature: "explicit".into(),
            dir: d.into(),
        },
        None => Instance::discover(None)?,
    })
}

fn eis_locks() -> usize {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.starts_with("eis-") && n.ends_with(".lock")
                })
                .count()
        })
        .unwrap_or(0)
}

/// What the EIS thread prints for one event. Keymaps are big: print a summary.
fn show(ev: &SourceEvent) -> String {
    match ev {
        SourceEvent::Keymap { text } => {
            let symbols = text
                .lines()
                .find(|l| l.contains("xkb_symbols"))
                .unwrap_or("")
                .trim();
            format!("Keymap {{ len: {}, {symbols} }}", text.len())
        }
        SourceEvent::Key {
            code,
            pressed,
            t_eis,
        } => format!(
            "Key {{ code: {code}, pressed: {pressed}, t_eis: {}, age_us: {} }}",
            t_eis.0,
            mono_us().saturating_sub(t_eis.0)
        ),
        other => format!("{other:?}"),
    }
}

/// The `capture` thread: a calloop loop that takes `(generation, fd)` pairs
/// and runs one EisSource at a time, printing events and reporting the end
/// to `ctl` with the generation, so `ctl` can ignore the end of a session it
/// has already replaced.
fn spawn_capture(
    ends: mpsc::UnboundedSender<(u64, EisEnd)>,
) -> Res<calloop::channel::Sender<(u64, OwnedFd)>> {
    let (tx, rx) = calloop_channel::<(u64, OwnedFd)>();
    std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            let mut ev_loop: EventLoop<'static, ()> = EventLoop::try_new().expect("event loop");
            let handle = ev_loop.handle();
            let h2 = handle.clone();
            let mut current: Option<calloop::RegistrationToken> = None;
            handle
                .insert_source(rx, move |ev, _, _| {
                    let ChannelEvent::Msg((generation, fd)) = ev else {
                        return;
                    };
                    // One session at a time: the old source goes, unheard.
                    if let Some(old) = current.take() {
                        h2.remove(old);
                    }
                    let src = match EisSource::new(fd) {
                        Ok(s) => s,
                        Err(e) => {
                            out!("EIS_ERR new: {e}");
                            let _ = ends.send((generation, EisEnd::Error(e.to_string())));
                            return;
                        }
                    };
                    out!("EIS_SOURCE inserted generation={generation}");
                    let ends = ends.clone();
                    match h2.insert_source(src, move |o, _, _| match o {
                        EisOutput::Event(ev) => out!("EIS {}", show(&ev)),
                        EisOutput::KeyboardLost => out!("EIS_KEYBOARD_LOST"),
                        EisOutput::Ended(end) => {
                            out!("EIS_ENDED generation={generation} {end:?}");
                            let _ = ends.send((generation, end));
                        }
                    }) {
                        Ok(token) => current = Some(token),
                        Err(e) => out!("EIS_ERR insert: {e}"),
                    }
                })
                .expect("channel source");
            loop {
                if let Err(e) = ev_loop.dispatch(None, &mut ()) {
                    out!("EIS_ERR dispatch: {e}");
                }
            }
        })?;
    Ok(tx)
}

struct Probe {
    cfg: Cfg,
    ipc: Instance,
    portal: Portal,
    fds: calloop::channel::Sender<(u64, OwnedFd)>,
    /// The EIS source generation in use: an end from an older one is stale.
    generation: u64,
    zone: Option<Zone>,
    /// The current activation: id and contact point.
    active: Option<(u32, f64, f64)>,
    self_disabled: bool,
    rezone_at: Option<tokio::time::Instant>,
    bounce_at: Option<tokio::time::Instant>,
    cursor_check_at: Option<tokio::time::Instant>,
    last_poll: Option<LockCheck>,
}

impl Probe {
    fn barrier(&self) -> Option<Barrier> {
        self.zone.map(|z| Barrier {
            id: 1,
            monitor: MonitorId {
                peer: PeerId([0; 32]),
                index: self.cfg.zone as u8,
            },
            side: self.cfg.side,
            pos: edge_barrier(self.cfg.side, z),
        })
    }

    /// GetZones, SetPointerBarriers (through `execute`, as ctl would), Enable.
    async fn arm(&mut self) -> Res<()> {
        let t0 = Instant::now();
        let zones = self.portal.refresh_zones().await?.to_vec();
        for (i, z) in zones.iter().enumerate() {
            out!("ZONE n={i} {z:?}");
        }
        self.zone = zones.get(self.cfg.zone).copied();
        let barrier = self.barrier().ok_or("no such zone")?;
        out!("BARRIER {barrier:?}");
        let set = self
            .portal
            .execute(&PortalCmd::SetBarriers {
                refresh_zones: false,
                barriers: vec![barrier],
            })
            .await?;
        out!("SOURCE_EVENT {set:?}");
        self.portal.execute(&PortalCmd::Enable).await?;
        self.self_disabled = false;
        out!("ARMED us={}", t0.elapsed().as_micros());
        Ok(())
    }

    async fn release(&mut self, why: &str) {
        let Some((id, x, y)) = self.active.take() else {
            return;
        };
        let Some(zone) = self.zone else { return };
        let along = if self.cfg.side.is_vertical() { y } else { x };
        let pos = inside_point(self.cfg.side, zone, along, self.cfg.inset);
        let t0 = Instant::now();
        let r = self
            .portal
            .execute(&PortalCmd::Release {
                activation_id: Some(id),
                pos: Some(pos),
            })
            .await;
        out!(
            "RELEASE why={why} activation_id={id} pos=({},{}) us={} result={:?}",
            pos.x,
            pos.y,
            t0.elapsed().as_micros(),
            r.map(|_| ())
        );
        self.cursor_check_at = Some(tokio::time::Instant::now() + Duration::from_millis(50));
    }

    /// Close, IPC release, open, hand the fd over, arm.
    async fn recreate(&mut self, why: &str) -> Res<()> {
        out!("RECREATE why={why} eis_locks={}", eis_locks());
        self.active = None;
        self.bounce_at = None;
        self.rezone_at = None;
        let t0 = Instant::now();
        let r = release_input_capture(&self.ipc);
        out!("IPC_RELEASE result={r:?}");
        let (portal, fd) = Portal::open().await?;
        let old = std::mem::replace(&mut self.portal, portal);
        old.close().await;
        self.generation += 1;
        self.fds.send((self.generation, fd))?;
        out!(
            "SESSION session={} us={} eis_locks={}",
            self.portal.session_path(),
            t0.elapsed().as_micros(),
            eis_locks()
        );
        self.arm().await
    }

    async fn on_signal(&mut self, sig: PortalSignal) -> Res<()> {
        match sig {
            PortalSignal::Activated { .. } => {
                let t0 = Instant::now();
                let lock = lock_check_async(&self.ipc, LOCK_CHECK_TIMEOUT).await;
                out!("LOCK_CHECK {lock:?} us={}", t0.elapsed().as_micros());
                let ev = sig.into_source_event(lock).expect("not Lost");
                out!("SOURCE_EVENT {ev:?}");
                if let SourceEvent::Activated {
                    activation_id,
                    x,
                    y,
                    ..
                } = ev
                {
                    self.active = Some((activation_id, x, y));
                    if lock != LockCheck::Unlocked {
                        self.release("lock").await;
                    } else if let Some(d) = self.cfg.bounce_after {
                        self.bounce_at = Some(tokio::time::Instant::now() + d);
                    }
                }
            }
            PortalSignal::Lost(lost) => {
                out!("LOST {lost:?}");
                self.recreate(&format!("{lost:?}")).await?;
            }
            other => {
                let rezone = matches!(other, PortalSignal::Disabled | PortalSignal::ZonesChanged);
                let ev = other
                    .into_source_event(LockCheck::Unknown)
                    .expect("not Lost");
                out!(
                    "SOURCE_EVENT {ev:?}{}",
                    if self.self_disabled {
                        " (after own Disable)"
                    } else {
                        ""
                    }
                );
                if matches!(ev, SourceEvent::Deactivated { .. }) || rezone {
                    self.active = None;
                    self.bounce_at = None;
                }
                if rezone && !self.self_disabled {
                    self.rezone_at = Some(tokio::time::Instant::now() + Duration::from_millis(100));
                }
            }
        }
        Ok(())
    }
}

async fn run(cfg: Cfg) -> Res<()> {
    let ipc = ipc_for(cfg.ipc_dir.as_deref())?;
    out!("START cfg={cfg:?} eis_locks={}", eis_locks());
    // Before any session.
    let t0 = Instant::now();
    let r = release_input_capture(&ipc);
    out!("IPC_RELEASE us={} result={r:?}", t0.elapsed().as_micros());

    let (ends_tx, mut ends) = mpsc::unbounded_channel();
    let fds = spawn_capture(ends_tx)?;
    let t0 = Instant::now();
    let (portal, fd) = Portal::open().await?;
    out!(
        "SESSION session={} us={} eis_locks={}",
        portal.session_path(),
        t0.elapsed().as_micros(),
        eis_locks()
    );
    fds.send((0, fd))?;

    let mut logind = if cfg.logind {
        match Logind::connect().await {
            Ok(l) => Some(l),
            Err(e) => {
                out!("LOGIND_ERR {e}");
                None
            }
        }
    } else {
        None
    };
    let mut inhibitor = match &logind {
        Some(l) => l
            .inhibit_sleep()
            .await
            .map_err(|e| out!("INHIBIT_ERR {e}"))
            .ok(),
        None => None,
    };
    out!("INHIBIT held={}", inhibitor.is_some());

    let poll = cfg.poll;
    let mut p = Probe {
        cfg,
        ipc,
        portal,
        fds,
        generation: 0,
        zone: None,
        active: None,
        self_disabled: false,
        rezone_at: None,
        bounce_at: None,
        cursor_check_at: None,
        last_poll: None,
    };
    p.arm().await?;
    out!("READY");

    let mut usr1 = signal(SignalKind::user_defined1())?;
    let mut usr2 = signal(SignalKind::user_defined2())?;
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut poll_at = poll.map(|d| tokio::time::Instant::now() + d);

    loop {
        tokio::select! {
            sig = p.portal.next_signal() => p.on_signal(sig).await?,
            Some((generation, end)) = ends.recv() => {
                if generation == p.generation {
                    out!("EIS_END {end:?}");
                    p.recreate(&format!("eis {end:?}")).await?;
                } else {
                    out!("EIS_END stale generation={generation} {end:?}");
                }
            }
            Some(ev) = async { match logind.as_mut() { Some(l) => l.next().await, None => std::future::pending().await } } => {
                out!("SOURCE_EVENT {ev:?}");
                if let SourceEvent::PrepareForSleep { start } = ev {
                    if start {
                        p.release("suspend").await;
                        drop(inhibitor.take());
                        out!("INHIBIT released");
                    } else {
                        if let Some(l) = &logind {
                            inhibitor = l.inhibit_sleep().await.map_err(|e| out!("INHIBIT_ERR {e}")).ok();
                        }
                        let lock = lock_check_async(&p.ipc, LOCK_CHECK_TIMEOUT).await;
                        out!("INHIBIT held={} LOCK_CHECK {lock:?}", inhibitor.is_some());
                    }
                }
            }
            _ = sleep_until_opt(p.bounce_at) => {
                p.bounce_at = None;
                p.release("bounce").await;
            }
            _ = sleep_until_opt(p.cursor_check_at) => {
                p.cursor_check_at = None;
                out!("CURSORPOS {:?}", p.ipc.cursor_pos());
            }
            _ = sleep_until_opt(p.rezone_at) => {
                p.rezone_at = None;
                if let Err(e) = p.arm().await {
                    out!("REZONE_ERR {e}");
                    p.recreate("rezone failed").await?;
                }
            }
            _ = sleep_until_opt(poll_at) => {
                poll_at = poll.map(|d| tokio::time::Instant::now() + d);
                let t0 = Instant::now();
                let lock = lock_check_async(&p.ipc, LOCK_CHECK_TIMEOUT).await;
                if p.last_poll != Some(lock) {
                    out!("LOCK_POLL {lock:?} us={}", t0.elapsed().as_micros());
                    p.last_poll = Some(lock);
                }
            }
            _ = usr1.recv() => {
                p.self_disabled = true;
                let r = p.portal.execute(&PortalCmd::Disable).await;
                out!("OWN_DISABLE result={:?}", r.map(|_| ()));
            }
            _ = usr2.recv() => {
                out!("REARM (SIGUSR2)");
                p.arm().await?;
            }
            _ = term.recv() => { out!("SIGTERM"); break; }
            _ = int.recv() => { out!("SIGINT"); break; }
        }
    }
    p.release("exit").await;
    p.portal.close().await;
    out!("EXIT eis_locks={}", eis_locks());
    Ok(())
}

fn lock_cmd(args: &[String]) -> Res<()> {
    let mut n = 10u32;
    let mut interval = Duration::from_millis(100);
    let mut dir = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--n" => n = val()?.parse()?,
            "--interval-ms" => interval = Duration::from_millis(val()?.parse()?),
            "--ipc-dir" => dir = Some(val()?),
            other => return Err(format!("unknown arg {other}").into()),
        }
    }
    let ipc = ipc_for(dir.as_deref())?;
    for i in 0..n {
        let t0 = Instant::now();
        let lock = lock_check(&ipc, LOCK_CHECK_TIMEOUT);
        out!("LOCK_CHECK i={i} {lock:?} us={}", t0.elapsed().as_micros());
        std::thread::sleep(interval);
    }
    Ok(())
}

fn observe(args: &[String]) -> Res<()> {
    let interval: u64 = args.first().map(|s| s.parse()).transpose()?.unwrap_or(1000);
    let secs: u64 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(10);
    let ipc = Instance::discover(None)?;
    let end = Instant::now() + Duration::from_secs(secs);
    let mut last = None;
    while Instant::now() < end {
        let p = ipc.cursor_pos().ok().map(|c| (c.x, c.y));
        if p != last {
            out!("CURSOR {p:?}");
            last = p;
        }
        std::thread::sleep(Duration::from_micros(interval));
    }
    Ok(())
}

/// `run` takes input capture and `release` and `observe` act on the live
/// compositor, so none of them may start outside a lab VM. `FERRY_LAB=1` overrides.
fn refuse_outside_lab() {
    let in_qemu = ["product_name", "sys_vendor"].iter().any(|f| {
        std::fs::read_to_string(format!("/sys/class/dmi/id/{f}"))
            .map(|s| s.contains("QEMU") || s.contains("KVM") || s.contains("Standard PC"))
            .unwrap_or(false)
    });
    if in_qemu || std::env::var_os("FERRY_LAB").is_some_and(|v| v == "1") {
        return;
    }
    eprintln!(
        "refusing: capture-probe captures input and is for a lab VM only (DMI is not QEMU/KVM). \
         Set FERRY_LAB=1 to override on a machine you have cleared for it"
    );
    std::process::exit(2);
}

fn main() -> Res<()> {
    refuse_outside_lab();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (sub, rest) = args
        .split_first()
        .map_or(("", &[][..]), |(s, r)| (s.as_str(), r));
    match sub {
        "run" => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(run(parse_run(rest)?)),
        "lock" => lock_cmd(rest),
        "release" => {
            let ipc = Instance::discover(None)?;
            let t0 = Instant::now();
            let r = release_input_capture(&ipc);
            out!("IPC_RELEASE us={} result={r:?}", t0.elapsed().as_micros());
            Ok(())
        }
        "observe" => observe(rest),
        _ => {
            eprintln!(
                "usage: capture-probe run --edge left|right|top|bottom [--zone N] [--inset PX] \
                 [--bounce-after-ms MS | --hold] [--poll-ms MS] [--ipc-dir DIR] [--no-logind]\n\
                 \x20      capture-probe lock [--n N] [--interval-ms MS] [--ipc-dir DIR]\n\
                 \x20      capture-probe release\n\
                 \x20      capture-probe observe [interval_us] [seconds]"
            );
            std::process::exit(2)
        }
    }
}
