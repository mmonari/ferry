//! Keeps the pointer's [`Layout`] equal to the compositor's monitors.
//!
//! One thread, blocked in `poll` on two fds: Hyprland's event socket and a
//! wake-up socket the inject thread writes to when a `wl_output` changes.
//! Hyprland 0.56.2 emits no event for a mode, scale or position change made
//! through `hl.monitor`, so `socket2` alone would leave those unseen.
//!
//! A burst of changes is debounced: a hot-plug passes through a transient 1x1
//! output for ~70 ms, and a layout read in that window is wrong.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use calloop::channel::SyncSender;
use ferry_hypr_ipc::{EventStream, Instance};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use std::sync::mpsc::TrySendError;

use crate::{Error, InputCmd, Layout, Result, Shared, Target};

/// Quiet time after the last change before the layout is read.
const DEBOUNCE: Duration = Duration::from_millis(100);

pub(crate) struct Follower {
    stop: Arc<AtomicBool>,
    wake: UnixStream,
    join: std::thread::JoinHandle<()>,
}

impl Follower {
    pub(crate) fn stop(self) {
        // Release pairs with the Acquire load after the wake byte: the follower must not wake
        // and still read `false`.
        self.stop.store(true, Ordering::Release);
        let _ = std::io::Write::write(&mut &self.wake, &[1]);
        let _ = self.join.join();
    }
}

pub(crate) fn spawn(
    target: &Target,
    tx: SyncSender<InputCmd>,
    shared: Arc<Shared>,
) -> Result<Follower> {
    let instance = target.instance()?;
    // Subscribe before the first read, so a change in between is not lost.
    let events = instance.events()?;
    let layout = read_layout(&instance)?;
    let (wake_tx, wake_rx) = UnixStream::pair()?;
    wake_tx.set_nonblocking(true)?;
    wake_rx.set_nonblocking(true)?;
    *shared
        .layout_wake
        .lock()
        .map_err(|_| Error::Input("layout wake lock poisoned".into()))? =
        Some(wake_tx.try_clone()?);
    send(&tx, &shared, &layout)?;
    tracing::info!(?layout, "following the monitor layout");
    let stop = Arc::new(AtomicBool::new(false));
    let join = {
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("ferry-layout".into())
            .spawn(move || {
                if let Err(e) = run(&instance, events, wake_rx, &tx, &shared, layout, &stop) {
                    tracing::warn!(error = %e, "stopped following the monitor layout");
                }
                if let Ok(mut wake) = shared.layout_wake.lock() {
                    *wake = None;
                }
            })?
    };
    Ok(Follower {
        stop,
        wake: wake_tx,
        join,
    })
}

fn read_layout(instance: &Instance) -> Result<Layout> {
    let layout = Layout::from_monitors(&instance.monitors()?);
    if layout.outputs.is_empty() {
        return Err(Error::Input("Hyprland reports no enabled monitor".into()));
    }
    Ok(layout)
}

fn send(tx: &SyncSender<InputCmd>, shared: &Shared, layout: &Layout) -> Result<()> {
    match tx.try_send(InputCmd::SetLayout(layout.clone())) {
        Ok(()) => {
            Shared::bump(&shared.counters.layout_updates);
            Ok(())
        }
        Err(TrySendError::Full(_)) => {
            shared.overflow.store(true, Ordering::Relaxed);
            Err(Error::Overflow)
        }
        Err(TrySendError::Disconnected(_)) => Err(Error::ThreadGone),
    }
}

fn run(
    instance: &Instance,
    mut events: EventStream,
    mut wake: UnixStream,
    tx: &SyncSender<InputCmd>,
    shared: &Shared,
    mut current: Layout,
    stop: &AtomicBool,
) -> Result<()> {
    let mut changed_at: Option<Instant> = None;
    loop {
        let timeout = changed_at.map(|t| {
            let left = (t + DEBOUNCE).saturating_duration_since(Instant::now());
            Timespec {
                tv_sec: left.as_secs() as _,
                tv_nsec: left.subsec_nanos() as _,
            }
        });
        let mut fds = [
            PollFd::new(&events, PollFlags::IN),
            PollFd::new(&wake, PollFlags::IN),
        ];
        match poll(&mut fds, timeout.as_ref()) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(Error::Io(e.into())),
        }
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut buf = [0u8; 64];
        while matches!(wake.read(&mut buf), Ok(n) if n > 0) {
            changed_at = Some(Instant::now());
        }
        while let Some(event) = events.try_next_event()? {
            if event.affects_monitor_layout() {
                changed_at = Some(Instant::now());
            }
        }
        if changed_at.is_some_and(|t| t.elapsed() >= DEBOUNCE) {
            changed_at = None;
            match read_layout(instance) {
                Ok(layout) if layout != current => {
                    tracing::info!(?layout, "monitor layout changed");
                    send(tx, shared, &layout)?;
                    current = layout;
                }
                Ok(_) => {}
                // A read that fails is retried at the next change; the old
                // layout stays in force.
                Err(e) => tracing::warn!(error = %e, "cannot read the monitor layout"),
            }
        }
    }
}
