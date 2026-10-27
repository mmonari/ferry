//! A stand-in for the portal session, for in-process tests of the capture side (design m1-rest
//! §3.7 `session::both_sides_capture`). It answers `SetBarriers` with no failure and every lock
//! check with `Unlocked`, records every portal command, and lets a test feed the capture thread
//! what EIS and the portal would (a keymap, an `Activated`). It never touches the compositor,
//! the portal or any input device, so a daemon that uses it may run on the live desktop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use calloop::channel::SyncSender;
use ferry_core::source::{LockCheck, PortalCmd, SourceEvent};
use tokio::sync::mpsc;

use crate::chan::CaptureCmd;
use crate::live::SharedLive;
use crate::portal_driver::DriverMsg;

/// Clone it, hand one copy to the daemon (`Options::fake_portal`), keep the other.
#[derive(Clone, Default)]
pub struct FakePortal {
    log: Arc<Mutex<Vec<(Instant, PortalCmd)>>>,
    capture: Arc<Mutex<Option<SyncSender<CaptureCmd>>>>,
    /// [`FakePortal::hang`]: the next message is never answered, as a hung xdph would do.
    hang: Arc<AtomicBool>,
    hung: Arc<AtomicBool>,
    /// What [`FakePortal::set_cursor`] set: the cursor a `go` request reads.
    cursor: Arc<Mutex<Option<(f64, f64)>>>,
    /// Every key crossing's nudge (warp x, y, then dx, dy), in order.
    nudges: Arc<Mutex<Vec<Nudge>>>,
}

/// A key crossing's nudge: the warp's x, y, then the motion's dx, dy (local logical).
pub type Nudge = (f64, f64, f64, f64);

impl FakePortal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every portal command so far, in order.
    pub fn commands(&self) -> Vec<PortalCmd> {
        self.timed().into_iter().map(|(_, c)| c).collect()
    }

    /// Every portal command so far, with when it ran.
    pub fn timed(&self) -> Vec<(Instant, PortalCmd)> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// From the next message on, answer nothing and take nothing more: a portal call that never
    /// returns.
    pub fn hang(&self) {
        self.hang.store(true, Ordering::Relaxed);
    }

    /// The driver is stuck in the call [`FakePortal::hang`] asked for.
    pub fn hung(&self) -> bool {
        self.hung.load(Ordering::Relaxed)
    }

    /// The cursor position (local logical) that `go` reads; `None` reads as a failed read.
    pub fn set_cursor(&self, pos: Option<(f64, f64)>) {
        if let Ok(mut c) = self.cursor.lock() {
            *c = pos;
        }
    }

    pub(crate) fn cursor(&self) -> Option<(f64, f64)> {
        self.cursor.lock().ok().and_then(|c| *c)
    }

    /// Every nudge a key crossing asked for, in place of moving the real cursor.
    pub fn nudges(&self) -> Vec<Nudge> {
        self.nudges.lock().map(|n| n.clone()).unwrap_or_default()
    }

    pub(crate) fn nudged(&self, x: f64, y: f64, dx: f64, dy: f64) {
        if let Ok(mut n) = self.nudges.lock() {
            n.push((x, y, dx, dy));
        }
    }

    /// Feed the capture thread one event, as EIS or the portal would. `false` before the daemon
    /// reached start-up step 4, or when the channel is full.
    pub fn event(&self, ev: SourceEvent) -> bool {
        let tx = self.capture.lock().ok().and_then(|c| c.clone());
        tx.is_some_and(|tx| tx.try_send(CaptureCmd::Event(ev)).is_ok())
    }

    /// Serve the driver's messages until `Close` (start-up step 4 in place of the portal).
    pub(crate) async fn run(
        self,
        mut rx: mpsc::Receiver<DriverMsg>,
        capture: SyncSender<CaptureCmd>,
        live: SharedLive,
    ) {
        if let Ok(mut c) = self.capture.lock() {
            *c = Some(capture.clone());
        }
        let send = |ev| {
            let _ = capture.try_send(CaptureCmd::Event(ev));
        };
        while let Some(msg) = rx.recv().await {
            if self.hang.load(Ordering::Relaxed) {
                self.hung.store(true, Ordering::Relaxed);
                tracing::debug!(?msg, "fake portal: hangs");
                std::future::pending::<()>().await;
            }
            match msg {
                DriverMsg::Portal(cmd) => {
                    if let Ok(mut l) = self.log.lock() {
                        l.push((Instant::now(), cmd.clone()));
                    }
                    if let PortalCmd::SetBarriers { barriers, .. } = cmd {
                        {
                            let mut l = live.lock();
                            l.barriers = u32::try_from(barriers.len()).unwrap_or(u32::MAX);
                            l.failed_barriers.clear();
                        }
                        live.changed();
                        send(SourceEvent::BarriersSet { failed: Vec::new() });
                    }
                }
                DriverMsg::CheckLock => {
                    let was = live.lock().screen_locked.replace(false);
                    if was != Some(false) {
                        live.changed();
                    }
                    send(SourceEvent::LockPolled(LockCheck::Unlocked));
                }
                DriverMsg::Flush(ack) => {
                    let _ = ack.send(());
                }
                DriverMsg::Close(ack) => {
                    let _ = ack.send(());
                    return;
                }
                DriverMsg::EisEnded { .. } | DriverMsg::SleepHandled => {}
            }
        }
    }
}
