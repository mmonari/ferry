//! The daemon watchdog and the notifier it sends through.
//!
//! systemd kills a `Type=notify` service that does not send `WATCHDOG=1` within `WatchdogSec`.
//! The threads that can hold capture or keys are `capture` (an active crossing) and `net` (the
//! TargetMachine and its 650 ms watchdog). Each stores `CLOCK_MONOTONIC` in its own
//! [`Beats`] cell on every wake-up and wakes at least once per [`Beats::thread_period`] (1 s).
//! `ctl` ticks every [`Beats::period`] (500 ms, user decision of 2026-10-18) and sends
//! `WATCHDOG=1` only when both cells are younger than 1.2 thread periods, so a hang in either
//! thread stops the heartbeat, and its own tick proves `ctl` alive.
//!
//! Why two periods. `WatchdogSec=2s` kills a daemon that sends nothing for 2 s. With one send per
//! second, a single send skipped for a thread that was merely late killed a healthy ferryd; with
//! one per 500 ms a thread can be late by about 2 s before it does. The threads need not wake as
//! often as the sender: that would be 6 wake-ups/s at idle, against a heartbeat budget of 4
//! (NFR-RES-01 allows 10 for the whole connected daemon). So `ctl`
//! sends twice a second and the other two threads wake once a second (2 + 1 + 1).
//!
//! Without `WATCHDOG_USEC` (ferryd run by hand) nothing is sent; the threads still tick.

use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use ferry_capture::Logind;
use ferry_core::source::SourceEvent;
use sd_notify::NotifyState;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::clock;

/// The period of the heartbeat (`WATCHDOG=1` sends): 500 ms, with `WatchdogSec=2s`.
pub const PERIOD: Duration = Duration::from_millis(500);

/// The threads whose liveness the heartbeat vouches for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thread {
    Capture,
    Net,
}

/// `last_alive` of the `capture` and `net` threads, shared through [`crate::live::SharedLive`].
/// Lock-free: a thread that stalls must not be able to hold a lock `ctl` needs.
#[derive(Debug, Default)]
pub struct Beats {
    capture: AtomicU64,
    net: AtomicU64,
    /// µs; 0 means [`PERIOD`].
    period_us: AtomicU64,
    /// The shutdown order is running: the capture thread has already stopped on purpose.
    stopping: AtomicBool,
    /// Test hook: `CLOCK_MONOTONIC` µs until which a thread sleeps at its next wake-up.
    stall_capture: AtomicU64,
    stall_net: AtomicU64,
    /// Test hook: µs the net loop waits (asynchronously, so its producers keep filling its
    /// queues) after each event it handles, as a handler that awaits a slow thread would.
    drag_net: AtomicU64,
    /// Test hook: `CLOCK_MONOTONIC` µs until which `ctl`'s main loop leaves its queue unread
    /// (its other tasks, the socket and the portal driver, keep running).
    pause_ctl: AtomicU64,
    /// Test hook: the net thread panics at its next wake-up.
    panic_net: AtomicBool,
}

impl Beats {
    fn cell(&self, t: Thread) -> &AtomicU64 {
        match t {
            Thread::Capture => &self.capture,
            Thread::Net => &self.net,
        }
    }

    /// Before the threads start. Tests use a short period.
    pub fn set_period(&self, d: Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX).max(1);
        self.period_us.store(us, Ordering::Relaxed);
    }

    /// How often `ctl` ticks and may send `WATCHDOG=1`.
    pub fn period(&self) -> Duration {
        match self.period_us.load(Ordering::Relaxed) {
            0 => PERIOD,
            us => Duration::from_micros(us),
        }
    }

    /// How often the `capture` and `net` threads wake to beat: twice the heartbeat period.
    pub fn thread_period(&self) -> Duration {
        self.period() * 2
    }

    /// The calling thread is alive. Call it on every wake-up.
    pub fn beat(&self, t: Thread) {
        self.cell(t).store(clock::now().0, Ordering::Relaxed);
    }

    /// How long since `t` last called [`Beats::beat`]; `None` when it never has.
    pub fn age(&self, t: Thread) -> Option<Duration> {
        match self.cell(t).load(Ordering::Relaxed) {
            0 => None,
            at => Some(Duration::from_micros(clock::now().0.saturating_sub(at))),
        }
    }

    /// Both threads beat within the last 1.2 thread periods.
    pub fn healthy(&self) -> bool {
        let limit = self.thread_period().mul_f32(1.2);
        [Thread::Capture, Thread::Net]
            .into_iter()
            .all(|t| self.age(t).is_some_and(|a| a < limit))
    }

    /// The shutdown order has begun: the heartbeat goes on until `ctl` is done, because the
    /// order takes longer than one watchdog interval and a kill in the middle of it would end
    /// the portal session uncleanly.
    pub fn set_stopping(&self) {
        self.stopping.store(true, Ordering::Relaxed);
    }

    /// Whether `WATCHDOG=1` may go out now.
    pub fn may_notify(&self) -> bool {
        self.stopping.load(Ordering::Relaxed) || self.healthy()
    }

    /// The threads whose beat is too old for a send, with its age (`None`: never beat).
    pub fn stale(&self) -> Vec<(Thread, Option<Duration>)> {
        let limit = self.thread_period().mul_f32(1.2);
        [Thread::Capture, Thread::Net]
            .into_iter()
            .map(|t| (t, self.age(t)))
            .filter(|(_, a)| !a.is_some_and(|a| a < limit))
            .collect()
    }

    /// Test hook (`Daemon::stall`): make `t` sleep for `d` at its next wake-up.
    pub fn stall(&self, t: Thread, d: Duration) {
        let until = clock::now().0 + u64::try_from(d.as_micros()).unwrap_or(u64::MAX / 2);
        match t {
            Thread::Capture => self.stall_capture.store(until, Ordering::Relaxed),
            Thread::Net => self.stall_net.store(until, Ordering::Relaxed),
        }
    }

    /// Test hook (`Daemon::drag_net`): the net loop waits `d` after every event it handles, until
    /// called again with zero.
    pub fn drag_net(&self, d: Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        self.drag_net.store(us, Ordering::Relaxed);
    }

    /// The net loop's wait after each event, when the test hook asks for one.
    pub fn net_drag(&self) -> Option<Duration> {
        match self.drag_net.load(Ordering::Relaxed) {
            0 => None,
            us => Some(Duration::from_micros(us)),
        }
    }

    /// Test hook (`Daemon::pause_ctl`): `ctl`'s main loop reads nothing from its queue for `d`.
    pub fn pause_ctl(&self, d: Duration) {
        let until = clock::now().0 + u64::try_from(d.as_micros()).unwrap_or(u64::MAX / 2);
        self.pause_ctl.store(until, Ordering::Relaxed);
    }

    /// How long `ctl`'s main loop still has to leave its queue alone, when paused.
    pub fn ctl_paused(&self) -> Option<Duration> {
        let left = self
            .pause_ctl
            .load(Ordering::Relaxed)
            .saturating_sub(clock::now().0);
        (left > 0).then(|| Duration::from_micros(left))
    }

    /// Test hook (`Daemon::panic_net`): the net thread panics at its next wake-up.
    pub fn panic_net(&self) {
        self.panic_net.store(true, Ordering::Relaxed);
    }

    /// Whether the net thread was asked to panic (once).
    pub fn take_panic_net(&self) -> bool {
        self.panic_net.swap(false, Ordering::Relaxed)
    }

    /// Called by a thread right after it beats: sleeps if a stall was asked for.
    pub fn maybe_stall(&self, t: Thread) {
        let cell = match t {
            Thread::Capture => &self.stall_capture,
            Thread::Net => &self.stall_net,
        };
        let until = cell.swap(0, Ordering::Relaxed);
        if until != 0 {
            let left = until.saturating_sub(clock::now().0);
            std::thread::sleep(Duration::from_micros(left));
        }
    }
}

/// Where `READY=1`, `STOPPING=1` and `WATCHDOG=1` go.
#[derive(Debug, Clone, Default)]
pub enum Notifier {
    /// Nowhere.
    #[default]
    Off,
    /// `$NOTIFY_SOCKET`, as systemd sets it.
    Env,
    /// An explicit datagram socket path (tests; a leading `@` is an abstract socket).
    Socket(PathBuf),
}

impl Notifier {
    pub fn send(&self, states: &[NotifyState]) -> io::Result<()> {
        match self {
            Notifier::Off => Ok(()),
            Notifier::Env => sd_notify::notify(states),
            Notifier::Socket(path) => {
                let msg: String = states.iter().map(|s| format!("{s}\n")).collect();
                send_to(path, msg.as_bytes())
            }
        }
    }
}

fn send_to(path: &std::path::Path, msg: &[u8]) -> io::Result<()> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::SocketAddr;
    let sock = UnixDatagram::unbound()?;
    let bytes: &[u8] = path.as_os_str().as_bytes();
    let addr = match bytes.strip_prefix(b"@") {
        Some(name) => SocketAddr::from_abstract_name(name)?,
        None => SocketAddr::from_pathname(path)?,
    };
    sock.send_to_addr(msg, &addr).map(|_| ())
}

/// The systemd service unit this process runs in: the leaf of its cgroup v2 path, when that is
/// a `.service`. `NOTIFY_SOCKET` and `INVOCATION_ID` cannot say it, because a terminal inside a
/// unit passes them on to everything it runs, and a ferryd started by hand there is not in a
/// service (its leaf is the terminal's `.scope`).
pub fn own_service() -> Option<String> {
    service_of_cgroup(&std::fs::read_to_string("/proc/self/cgroup").ok()?)
}

/// The `.service` leaf of the unified (`0::`) line of a `/proc/<pid>/cgroup` text.
pub fn service_of_cgroup(text: &str) -> Option<String> {
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    let leaf = path.trim_end().rsplit('/').next()?;
    leaf.ends_with(".service").then(|| leaf.to_string())
}

/// The interval systemd asked for (`WATCHDOG_USEC`, for this process), when it did.
pub fn from_env() -> Option<Duration> {
    sd_notify::watchdog_enabled()
}

/// The heartbeat period for a watchdog interval: a quarter of it, but at most [`PERIOD`]. The
/// interval of 2 s gives 500 ms.
pub fn period_for(interval: Duration) -> Duration {
    (interval / 4).min(PERIOD).max(Duration::from_millis(1))
}

/// How long systemd may go without `WATCHDOG=1` while the machine sleeps: from logind's
/// `PrepareForSleep(true)` to `(false)`.
///
/// systemd-sleep freezes `user.slice` before it suspends and thaws it after the resume, and the
/// user manager that runs this unit is frozen with ferryd. `CLOCK_MONOTONIC` stops only in the
/// sleep state proper, so the sleep hooks and device suspend and resume on either side of it count
/// against `WatchdogSec`: on macarchy the session stayed frozen 0.9, 3.05, 5.4 and 13.3 s. At the
/// thaw the user manager finds the timer expired and fires it within milliseconds, before
/// ferryd's threads have run (2.5 ms on 2026-11-16, PID 1259). So before the freeze the heartbeat
/// raises the limit to this, and puts it back once the machine is awake.
pub const SLEEP_LIMIT: Duration = Duration::from_secs(60);

/// How long the heartbeat waits for logind, connecting or taking its inhibitor.
const LOGIND_TIMEOUT: Duration = Duration::from_secs(1);

/// logind's `PrepareForSleep`, for the heartbeat. The `inhibitor` of a `start` is the delay
/// inhibitor that holds the sleep back; the heartbeat drops it once systemd has the longer limit.
#[derive(Debug)]
pub struct SleepEvent {
    pub start: bool,
    pub inhibitor: Option<OwnedFd>,
}

/// What the heartbeat needs besides the beats.
#[derive(Debug)]
pub struct Watch {
    /// `WatchdogSec`: the limit to put back after a sleep.
    pub interval: Duration,
    /// Subscribe to logind's `PrepareForSleep` (a real service). Tests send to `sleeps` instead.
    pub logind: bool,
    pub sleeps_tx: mpsc::UnboundedSender<SleepEvent>,
    pub sleeps: mpsc::UnboundedReceiver<SleepEvent>,
}

impl Watch {
    pub fn new(interval: Duration, logind: bool) -> Self {
        let (sleeps_tx, sleeps) = mpsc::unbounded_channel();
        Self {
            interval,
            logind,
            sleeps_tx,
            sleeps,
        }
    }
}

/// Where the limit systemd applies stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limit {
    /// `WatchdogSec`, or the interval put back after a sleep.
    Normal,
    /// [`SLEEP_LIMIT`] is in force. It goes back to the interval at `back` at the latest, or at
    /// the first tick with both threads alive once `woken`.
    Sleep { back: Instant, woken: bool },
}

struct Heart {
    beats: Arc<Beats>,
    notifier: Notifier,
    interval: Duration,
    limit: Limit,
}

fn usec(d: Duration) -> u32 {
    u32::try_from(d.as_micros()).unwrap_or(u32::MAX)
}

impl Heart {
    fn send(&self, states: &[NotifyState]) {
        if let Err(e) = self.notifier.send(states) {
            tracing::warn!(error = %e, ?states, "sd_notify failed");
        }
    }

    fn tick(&mut self) {
        if let Limit::Sleep { back, woken } = self.limit
            && ((woken && self.beats.may_notify()) || Instant::now() >= back)
        {
            self.restore();
            return;
        }
        if !self.beats.may_notify() {
            // Name the thread that withheld the heartbeat.
            for (thread, age) in self.beats.stale() {
                tracing::debug!(
                    ?thread,
                    age_ms = age.map(|a| a.as_millis() as u64),
                    "WATCHDOG=1 skipped: stale beat"
                );
            }
            return;
        }
        self.send(&[NotifyState::Watchdog]);
    }

    /// `WATCHDOG_USEC` back to the interval. `WATCHDOG=1` goes with it only when both threads
    /// are alive: a thread that hung across the sleep is then killed one interval later.
    fn restore(&mut self) {
        let healthy = self.beats.may_notify();
        let usec = NotifyState::WatchdogUsec(usec(self.interval));
        if healthy {
            self.send(&[usec, NotifyState::Watchdog]);
        } else {
            self.send(&[usec]);
        }
        self.limit = Limit::Normal;
        tracing::info!(
            interval_ms = self.interval.as_millis() as u64,
            healthy,
            "watchdog restored after sleep"
        );
    }

    fn sleep(&mut self, ev: SleepEvent) {
        let now = Instant::now();
        if ev.start {
            // Before the inhibitor goes: logind then waits, and the user manager has this
            // datagram in its queue before systemd-sleep freezes it.
            let usec = NotifyState::WatchdogUsec(usec(SLEEP_LIMIT));
            if self.beats.may_notify() {
                self.send(&[usec, NotifyState::Watchdog]);
            } else {
                self.send(&[usec]);
            }
            drop(ev.inhibitor);
            self.limit = Limit::Sleep {
                back: now + SLEEP_LIMIT,
                woken: false,
            };
            tracing::info!(
                limit_s = SLEEP_LIMIT.as_secs(),
                "watchdog extended for sleep"
            );
        } else if let Limit::Sleep { back, .. } = self.limit {
            // Back at the first tick with both threads alive, and within one thread period
            // whatever they say.
            self.limit = Limit::Sleep {
                back: back.min(now + self.beats.thread_period()),
                woken: true,
            };
            if self.beats.may_notify() {
                self.restore();
            }
        }
    }
}

/// Spawns the heartbeat on `ctl`'s `LocalSet`, and the logind feed when `watch.logind` says so.
/// Returns the feed: `ctl` must abort and await it before its runtime stops, because zbus spawns
/// onto the runtime when its objects drop, and outside one that panics (in a destructor: abort).
pub fn spawn(
    beats: Arc<Beats>,
    notifier: Notifier,
    watch: Watch,
) -> Option<tokio::task::JoinHandle<()>> {
    let Watch {
        interval,
        logind,
        sleeps_tx,
        sleeps,
    } = watch;
    let feed = logind.then(|| tokio::task::spawn_local(logind_sleeps(sleeps_tx)));
    tokio::task::spawn_local(heartbeat(beats, notifier, interval, sleeps));
    feed
}

/// The `ctl` task: one tick per period; `WATCHDOG=1` if both threads are alive. Across a sleep
/// the limit is raised to [`SLEEP_LIMIT`] and put back on wake.
async fn heartbeat(
    beats: Arc<Beats>,
    notifier: Notifier,
    interval: Duration,
    mut sleeps: mpsc::UnboundedReceiver<SleepEvent>,
) {
    let mut tick = crate::ticker::Ticker::new(beats.period());
    let mut heart = Heart {
        beats,
        notifier,
        interval,
        limit: Limit::Normal,
    };
    loop {
        let back = match heart.limit {
            Limit::Sleep { back, .. } => Some(back),
            Limit::Normal => None,
        };
        tokio::select! {
            periods = tick.tick() => {
                if periods > 1 {
                    // Ctl itself did not run for a while (a stall, or a frozen process).
                    tracing::debug!(
                        late_ms = heart
                            .beats
                            .period()
                            .saturating_mul(u32::try_from(periods - 1).unwrap_or(u32::MAX))
                            .as_millis() as u64,
                        "watchdog tick late"
                    );
                }
                heart.tick();
            }
            Some(ev) = sleeps.recv() => heart.sleep(ev),
            () = sleep_until(back) => heart.tick(),
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

/// logind's `PrepareForSleep`, with a delay inhibitor of the heartbeat's own: the portal
/// driver's goes once capture is released, and it is not there at all with capture off.
async fn logind_sleeps(tx: mpsc::UnboundedSender<SleepEvent>) {
    let mut logind = match tokio::time::timeout(LOGIND_TIMEOUT, Logind::connect()).await {
        Ok(Ok(l)) => l,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "no logind: the watchdog is not extended across sleep");
            return;
        }
        Err(_) => {
            tracing::warn!("logind did not answer: the watchdog is not extended across sleep");
            return;
        }
    };
    let mut inhibitor = inhibit(&logind).await;
    tracing::info!(
        inhibitor = inhibitor.is_some(),
        "watchdog follows logind sleep"
    );
    while let Some(ev) = logind.next().await {
        let SourceEvent::PrepareForSleep { start } = ev else {
            continue;
        };
        let held = if start { inhibitor.take() } else { None };
        if tx
            .send(SleepEvent {
                start,
                inhibitor: held,
            })
            .is_err()
        {
            return;
        }
        if !start {
            inhibitor = inhibit(&logind).await;
        }
    }
    tracing::warn!("logind connection gone: the watchdog is no longer extended across sleep");
}

async fn inhibit(logind: &Logind) -> Option<OwnedFd> {
    let why = "extend the systemd watchdog across sleep";
    match tokio::time::timeout(LOGIND_TIMEOUT, logind.inhibit_sleep_why(why)).await {
        Ok(Ok(fd)) => Some(fd),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "no sleep inhibitor for the watchdog");
            None
        }
        Err(_) => {
            tracing::warn!("no sleep inhibitor for the watchdog: logind did not answer");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_is_the_leaf_of_the_unified_cgroup() {
        let unit =
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/ferryd-m1exit.service\n";
        assert_eq!(
            service_of_cgroup(unit).as_deref(),
            Some("ferryd-m1exit.service")
        );
        // A terminal's scope inherits NOTIFY_SOCKET and INVOCATION_ID, but it is no service.
        let scope = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-graphical.slice/kitty-34533-0.scope\n";
        assert_eq!(service_of_cgroup(scope), None);
        // cgroup v1 lines are not read; no unified line, no service.
        assert_eq!(
            service_of_cgroup("1:name=systemd:/user.slice/ferryd.service\n"),
            None
        );
        assert_eq!(service_of_cgroup(""), None);
    }

    #[test]
    fn a_thread_that_never_beat_is_not_healthy() {
        let b = Beats::default();
        assert!(!b.healthy());
        b.beat(Thread::Capture);
        assert!(!b.healthy(), "net has not beaten");
        b.beat(Thread::Net);
        assert!(b.healthy());
    }

    #[test]
    fn a_beat_goes_stale_after_one_and_a_fifth_thread_periods() {
        let b = Beats::default();
        // the heartbeat period 50 ms: the threads wake every 100 ms, and 120 ms is stale
        b.set_period(Duration::from_millis(50));
        assert_eq!(b.thread_period(), Duration::from_millis(100));
        b.beat(Thread::Capture);
        b.beat(Thread::Net);
        assert!(b.healthy());
        std::thread::sleep(Duration::from_millis(60));
        b.beat(Thread::Net);
        assert!(b.healthy(), "60 ms old is under 120 ms");
        std::thread::sleep(Duration::from_millis(100));
        b.beat(Thread::Net);
        assert!(!b.healthy(), "capture is now 160 ms old");
        assert!(b.age(Thread::Capture).unwrap() >= Duration::from_millis(150));
    }

    #[test]
    fn stopping_sends_whatever_the_threads_say() {
        let b = Beats::default();
        assert!(!b.may_notify());
        b.set_stopping();
        assert!(b.may_notify());
    }

    #[test]
    fn the_period_is_a_quarter_of_the_interval_but_at_most_500_ms() {
        // WatchdogSec=2s: a send every 500 ms, the threads wake every second
        assert_eq!(
            period_for(Duration::from_secs(2)),
            Duration::from_millis(500)
        );
        assert_eq!(
            period_for(Duration::from_secs(30)),
            Duration::from_millis(500)
        );
        assert_eq!(
            period_for(Duration::from_millis(400)),
            Duration::from_millis(100)
        );
        assert_eq!(Beats::default().period(), PERIOD);
        assert_eq!(Beats::default().thread_period(), Duration::from_secs(1));
    }

    /// A wake that never comes (`PrepareForSleep(false)` lost): the limit still goes back at
    /// [`SLEEP_LIMIT`], and only vouches for threads that are alive.
    #[test]
    fn the_sleep_limit_goes_back_without_a_wake() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.sock");
        let sock = UnixDatagram::bind(&path).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let recv = || {
            let mut buf = [0u8; 128];
            let n = sock.recv(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let beats = Arc::new(Beats::default());
            let mut heart = Heart {
                beats: beats.clone(),
                notifier: Notifier::Socket(path.clone()),
                interval: Duration::from_secs(2),
                limit: Limit::Normal,
            };
            heart.sleep(SleepEvent {
                start: true,
                inhibitor: None,
            });
            assert_eq!(
                recv(),
                "WATCHDOG_USEC=60000000\n",
                "threads never beat: no WATCHDOG=1"
            );
            heart.tick();
            assert!(
                matches!(heart.limit, Limit::Sleep { .. }),
                "not before the deadline"
            );
            heart.limit = Limit::Sleep {
                back: Instant::now(),
                woken: false,
            };
            heart.tick();
            assert_eq!(heart.limit, Limit::Normal);
            assert_eq!(recv(), "WATCHDOG_USEC=2000000\n");
            beats.beat(Thread::Capture);
            beats.beat(Thread::Net);
            heart.tick();
            assert_eq!(recv(), "WATCHDOG=1\n");
        });
    }

    #[test]
    fn a_stall_sleeps_once() {
        let b = Beats::default();
        b.stall(Thread::Net, Duration::from_millis(80));
        let t = std::time::Instant::now();
        b.maybe_stall(Thread::Capture);
        assert!(
            t.elapsed() < Duration::from_millis(40),
            "other thread unaffected"
        );
        b.maybe_stall(Thread::Net);
        assert!(t.elapsed() >= Duration::from_millis(70));
        let t = std::time::Instant::now();
        b.maybe_stall(Thread::Net);
        assert!(t.elapsed() < Duration::from_millis(40), "only once");
    }
}
