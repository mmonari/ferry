//! The capture side of `ctl`: the portal session, Hyprland IPC lock checks,
//! logind's `PrepareForSleep`. A local task on the `ctl` runtime that reads the capture thread's
//! `PortalCmd`s, `CheckLock`s and EIS ends from a queue of its own, in order: `ctl`'s main loop
//! and its queue are not on that path, so nothing they wait for can hold up a `Release`.
//!
//! **Session lifecycle**. One session per daemon lifetime, opened after the IPC
//! release. It is re-created only when the portal says it is `Lost`, when the EIS
//! stream of the *current* generation ends, or when `SetBarriers` fails (a failed re-zone).
//! Each re-creation runs the IPC release again, hands the capture thread the new EIS fd with a
//! new generation, then sends `SessionRestarted`.
//!
//! **Lock gate.** Every `Activated` gets a fresh lock read (50 ms at most) before it is
//! forwarded; unknown counts as locked in the machine. `CheckLock` is the machine's poll.
//!
//! **Sleep.** A logind delay inhibitor is held, so `PrepareForSleep(true)` reaches the machine
//! and its `Release` runs before the machine sleeps. The inhibitor goes when the capture thread
//! reports `SleepHandled` (its release is queued ahead of that), and is taken again on wake.
//!
//! **Every D-Bus call is bounded**. zbus sets no method timeout, so an xdph that is alive
//! but does not answer held the driver, and every message behind it, forever. A portal call
//! that takes longer than [`PORTAL_CALL_TIMEOUT`] is abandoned: the IPC release ends any
//! capture at once, then the session is re-created as for a failed `SetBarriers`.

use std::os::fd::OwnedFd;
use std::time::Duration;

use calloop::channel::SyncSender;
use ferry_capture::{Logind, Portal, PortalError, PortalSignal, release_input_capture};
use ferry_core::source::{LockCheck, PortalCmd, SourceEvent};
use ferry_hypr_ipc::Instance;
use tokio::sync::{mpsc, oneshot};

use crate::chan::CaptureCmd;
use crate::live::SharedLive;

/// From the capture thread, in order (and `Flush`, `Close` from `ctl`'s shutdown).
#[derive(Debug)]
pub enum DriverMsg {
    Portal(PortalCmd),
    CheckLock,
    EisEnded {
        generation: u64,
        end: ferry_capture::EisEnd,
    },
    /// The capture thread handled `PrepareForSleep(true)`; the release it caused is queued
    /// ahead of this, so the sleep inhibitor can go once this is reached.
    SleepHandled,
    /// Every message before this one has run.
    Flush(oneshot::Sender<()>),
    /// Close the session and stop.
    Close(oneshot::Sender<()>),
}

/// How long a sleep may wait for the machine's release before the inhibitor goes anyway.
const SLEEP_GRACE: Duration = Duration::from_secs(1);
/// Retry delays for a portal that cannot be opened.
const OPEN_RETRY: [u64; 4] = [2, 5, 10, 30];
/// The longest a portal call (`SetBarriers`, `Enable`, `Disable`, `Release`, `Close`) or a
/// logind call may take before the driver gives up on it. xdph answers in milliseconds;
/// a `Release` waiting behind a hung call is a lockout, which the IPC release then ends.
pub const PORTAL_CALL_TIMEOUT: Duration = Duration::from_secs(1);
/// Opening a session (`CreateSession`, `ConnectToEIS`) may take longer: nothing captures yet.
const PORTAL_OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// The portal session as the driver uses it. Production: [`Portal`]; the driver's tests put a
/// session that never answers in its place.
pub(crate) trait Session {
    fn path(&self) -> &str;
    async fn execute(&mut self, cmd: &PortalCmd) -> Result<Option<SourceEvent>, PortalError>;
    /// Cancel-safe.
    async fn next_signal(&mut self) -> PortalSignal;
    async fn close(self);
}

impl Session for Portal {
    fn path(&self) -> &str {
        self.session_path()
    }
    async fn execute(&mut self, cmd: &PortalCmd) -> Result<Option<SourceEvent>, PortalError> {
        Portal::execute(self, cmd).await
    }
    async fn next_signal(&mut self) -> PortalSignal {
        Portal::next_signal(self).await
    }
    async fn close(self) {
        Portal::close(self).await;
    }
}

/// Where sessions and logind come from.
pub(crate) trait Backend {
    type Session: Session;
    /// `CreateSession` → `ConnectToEIS`: the session and its EIS socket.
    async fn open(&mut self) -> Result<(Self::Session, OwnedFd), PortalError>;
    async fn logind(&mut self) -> Option<Logind>;
}

/// The session bus's portal and the system bus's logind.
pub(crate) struct Xdg;

impl Backend for Xdg {
    type Session = Portal;
    async fn open(&mut self) -> Result<(Portal, OwnedFd), PortalError> {
        Portal::open().await
    }
    async fn logind(&mut self) -> Option<Logind> {
        match Logind::connect().await {
            Ok(l) => Some(l),
            Err(e) => {
                tracing::warn!(error = %e, "no logind: suspend does not release first");
                None
            }
        }
    }
}

pub(crate) struct Driver<B: Backend = Xdg> {
    backend: B,
    ipc: Option<Instance>,
    /// `[safety] lock_check_budget_ms`: every lock check, per activation and per poll.
    lock_budget: Duration,
    capture: SyncSender<CaptureCmd>,
    live: SharedLive,
    portal: Option<B::Session>,
    generation: u64,
    open_failures: usize,
    /// When to try opening again after a failure.
    retry: Option<tokio::time::Instant>,
    logind: Option<Logind>,
    inhibitor: Option<OwnedFd>,
    sleep_deadline: Option<tokio::time::Instant>,
}

/// `release_input_capture()` over Hyprland IPC, on the blocking pool. `ferry release` and a full
/// driver queue run it on a task of their own, outside the driver's queue.
pub(crate) async fn ipc_release(ipc: Option<Instance>) {
    let Some(ipc) = ipc else {
        return;
    };
    let t0 = std::time::Instant::now();
    match tokio::task::spawn_blocking(move || release_input_capture(&ipc)).await {
        Ok(Ok(())) => tracing::info!(us = t0.elapsed().as_micros(), "IPC release_input_capture"),
        Ok(Err(e)) => tracing::warn!(error = %e, "IPC release_input_capture failed"),
        Err(e) => tracing::warn!(error = %e, "IPC release task"),
    }
}

impl Driver<Xdg> {
    pub(crate) fn new(
        ipc: Option<Instance>,
        lock_budget: Duration,
        capture: SyncSender<CaptureCmd>,
        live: SharedLive,
    ) -> Self {
        Self::with_backend(Xdg, ipc, lock_budget, capture, live)
    }
}

impl<B: Backend> Driver<B> {
    pub(crate) fn with_backend(
        backend: B,
        ipc: Option<Instance>,
        lock_budget: Duration,
        capture: SyncSender<CaptureCmd>,
        live: SharedLive,
    ) -> Self {
        Self {
            backend,
            ipc,
            lock_budget,
            capture,
            live,
            portal: None,
            generation: 0,
            open_failures: 0,
            retry: None,
            logind: None,
            inhibitor: None,
            sleep_deadline: None,
        }
    }

    /// Start-up step 4, then serve until `Close`.
    pub(crate) async fn run(mut self, mut rx: mpsc::Receiver<DriverMsg>) {
        // Before any session, clear a capture orphaned by a killed ferryd.
        ipc_release(self.ipc.clone()).await;
        self.open().await;
        if let Ok(logind) = tokio::time::timeout(PORTAL_CALL_TIMEOUT, self.backend.logind()).await {
            self.logind = logind;
        } else {
            tracing::warn!("logind did not answer within 1 s: suspend does not release first");
        }
        if self.logind.is_some() {
            self.take_inhibitor().await;
        }

        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(DriverMsg::Close(ack)) => {
                        if let Some(p) = self.portal.take() {
                            close(p).await;
                        }
                        let _ = ack.send(());
                        return;
                    }
                    Some(msg) => self.message(msg).await,
                    None => return,
                },
                sig = next_signal(&mut self.portal) => self.signal(sig).await,
                ev = next_sleep(&mut self.logind) => self.sleep(ev).await,
                () = sleep_until(self.retry) => {
                    self.retry = None;
                    self.open().await;
                    if self.portal.is_some() {
                        // Arm on the new session as on a restart.
                        self.event(SourceEvent::SessionRestarted);
                    }
                }
                () = sleep_until(self.sleep_deadline) => {
                    tracing::warn!("no release before sleep within 1 s: dropping the inhibitor");
                    self.sleep_deadline = None;
                    self.inhibitor = None;
                }
            }
        }
    }

    /// `CreateSession` → `ConnectToEIS`, and the fd to the capture thread with a new generation.
    async fn open(&mut self) -> bool {
        let t0 = std::time::Instant::now();
        let opened = match tokio::time::timeout(PORTAL_OPEN_TIMEOUT, self.backend.open()).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err("the portal did not answer within 5 s".to_string()),
        };
        match opened {
            Ok((portal, fd)) => {
                self.generation += 1;
                tracing::info!(
                    session = portal.path(),
                    generation = self.generation,
                    us = t0.elapsed().as_micros(),
                    "portal session open"
                );
                self.portal = Some(portal);
                self.open_failures = 0;
                self.live.lock().portal = crate::live::PortalHealth {
                    open: true,
                    failures: 0,
                    last_error: None,
                };
                self.live.changed();
                self.to_capture(CaptureCmd::Eis {
                    generation: self.generation,
                    fd,
                });
                true
            }
            Err(e) => {
                self.open_failures += 1;
                self.live.lock().portal = crate::live::PortalHealth {
                    open: false,
                    failures: self.open_failures as u32,
                    last_error: Some(e.clone()),
                };
                self.live.changed();
                let secs = OPEN_RETRY[(self.open_failures - 1).min(OPEN_RETRY.len() - 1)];
                self.retry = Some(tokio::time::Instant::now() + Duration::from_secs(secs));
                tracing::error!(error = %e, attempt = self.open_failures, retry_in_s = secs,
                    "cannot open the portal session");
                false
            }
        }
    }

    /// Lost, EIS EOF, or a failed re-zone: a new session.
    async fn recreate(&mut self, why: &str) {
        tracing::warn!(
            why,
            generation = self.generation,
            "re-creating the portal session"
        );
        self.live.bump("session_restarts");
        if let Some(old) = self.portal.take() {
            self.live.lock().portal.open = false;
            self.live.changed();
            close(old).await;
        }
        ipc_release(self.ipc.clone()).await;
        if self.open().await {
            self.event(SourceEvent::SessionRestarted);
        }
    }

    async fn message(&mut self, msg: DriverMsg) {
        match msg {
            DriverMsg::Portal(cmd) => self.execute(cmd).await,
            DriverMsg::CheckLock => {
                let lock = self.lock_check().await;
                self.event(SourceEvent::LockPolled(lock));
            }
            DriverMsg::EisEnded { generation, end } => {
                if generation == self.generation {
                    self.recreate(&format!("EIS ended: {end:?}")).await;
                } else {
                    tracing::debug!(
                        generation,
                        current = self.generation,
                        "stale EIS end ignored"
                    );
                }
            }
            DriverMsg::SleepHandled => {
                if self.sleep_deadline.take().is_some() {
                    self.inhibitor = None;
                    tracing::info!("released before sleep; inhibitor dropped");
                }
            }
            DriverMsg::Flush(ack) => {
                let _ = ack.send(());
            }
            DriverMsg::Close(_) => unreachable!("handled in run"),
        }
    }

    async fn execute(&mut self, cmd: PortalCmd) {
        let Some(portal) = self.portal.as_mut() else {
            // No session: a barrier set fails as a whole; the rest has nothing to act on.
            if let PortalCmd::SetBarriers { barriers, .. } = &cmd {
                let failed: Vec<u32> = barriers.iter().map(|b| b.id).collect();
                self.record_barriers(barriers.len(), &failed);
                self.event(SourceEvent::BarriersSet { failed });
            }
            return;
        };
        let t0 = std::time::Instant::now();
        let Ok(result) = tokio::time::timeout(PORTAL_CALL_TIMEOUT, portal.execute(&cmd)).await
        else {
            // A hung portal. End any capture now, then start over on a new session.
            tracing::warn!(?cmd, "the portal did not answer within 1 s");
            self.live.bump("portal_timeouts");
            ipc_release(self.ipc.clone()).await;
            self.recreate("a portal call timed out").await;
            return;
        };
        let us = t0.elapsed().as_micros();
        match (&cmd, result) {
            (
                PortalCmd::SetBarriers {
                    barriers,
                    refresh_zones,
                },
                Ok(Some(ev)),
            ) => {
                if let SourceEvent::BarriersSet { failed } = &ev {
                    tracing::info!(
                        n = barriers.len(),
                        refresh_zones,
                        ?failed,
                        us,
                        "SetBarriers"
                    );
                    self.record_barriers(barriers.len(), failed);
                }
                self.event(ev);
            }
            (PortalCmd::SetBarriers { .. }, Err(e)) => {
                tracing::warn!(error = %e, "SetBarriers failed");
                self.recreate("SetBarriers failed").await;
            }
            (cmd, Ok(_)) => tracing::debug!(?cmd, us, "portal"),
            (cmd, Err(e)) => tracing::warn!(?cmd, error = %e, "portal command failed"),
        }
    }

    fn record_barriers(&self, n: usize, failed: &[u32]) {
        {
            let mut l = self.live.lock();
            l.barriers = u32::try_from(n).unwrap_or(u32::MAX);
            l.failed_barriers = failed.to_vec();
        }
        self.live.changed();
    }

    async fn signal(&mut self, sig: PortalSignal) {
        if let PortalSignal::Lost(lost) = &sig {
            self.recreate(&format!("{lost:?}")).await;
            return;
        }
        let t0 = std::time::Instant::now();
        let ev = match &self.ipc {
            Some(ipc) => sig.checked(ipc, self.lock_budget).await,
            None => sig.into_source_event(LockCheck::Unknown),
        };
        match ev {
            Ok(ev) => {
                if let SourceEvent::Activated {
                    activation_id,
                    barrier_id,
                    x,
                    y,
                    lock,
                } = &ev
                {
                    tracing::debug!(
                        activation_id,
                        barrier_id,
                        x,
                        y,
                        ?lock,
                        check_us = t0.elapsed().as_micros(),
                        "Activated"
                    );
                    self.note_lock(*lock);
                }
                self.event(ev);
            }
            Err(lost) => self.recreate(&format!("{lost:?}")).await,
        }
    }

    async fn sleep(&mut self, ev: SourceEvent) {
        let SourceEvent::PrepareForSleep { start } = ev else {
            return;
        };
        tracing::info!(start, "PrepareForSleep");
        self.event(ev);
        if start {
            self.sleep_deadline = Some(tokio::time::Instant::now() + SLEEP_GRACE);
        } else {
            self.take_inhibitor().await;
        }
    }

    async fn take_inhibitor(&mut self) {
        if let Some(l) = &self.logind {
            match tokio::time::timeout(PORTAL_CALL_TIMEOUT, l.inhibit_sleep()).await {
                Ok(Ok(fd)) => self.inhibitor = Some(fd),
                Ok(Err(e)) => tracing::warn!(error = %e, "no sleep inhibitor"),
                Err(_) => tracing::warn!("no sleep inhibitor: logind did not answer within 1 s"),
            }
        }
    }

    async fn lock_check(&self) -> LockCheck {
        let lock = match &self.ipc {
            Some(ipc) => ferry_capture::lock_check_async(ipc, self.lock_budget).await,
            None => LockCheck::Unknown,
        };
        self.note_lock(lock);
        lock
    }

    fn note_lock(&self, lock: LockCheck) {
        let now = match lock {
            LockCheck::Locked => Some(true),
            LockCheck::Unlocked => Some(false),
            LockCheck::Unknown => None,
        };
        // polled every few hundred milliseconds: a change is news, a repeat is not
        let was = std::mem::replace(&mut self.live.lock().screen_locked, now);
        if was != now {
            self.live.changed();
        }
    }

    fn event(&self, ev: SourceEvent) {
        self.to_capture(CaptureCmd::Event(ev));
    }

    fn to_capture(&self, cmd: CaptureCmd) {
        if let Err(e) = self.capture.try_send(cmd) {
            self.live.bump("capture_queue_full");
            tracing::error!(error = %e, "ctl → capture channel full or closed");
        }
    }
}

/// `Session.Close`, bounded like every portal call.
async fn close<S: Session>(session: S) {
    if tokio::time::timeout(PORTAL_CALL_TIMEOUT, session.close())
        .await
        .is_err()
    {
        tracing::warn!("the portal did not close the session within 1 s");
    }
}

async fn next_signal<S: Session>(portal: &mut Option<S>) -> PortalSignal {
    match portal {
        Some(p) => p.next_signal().await,
        None => std::future::pending().await,
    }
}

async fn next_sleep(logind: &mut Option<Logind>) -> SourceEvent {
    match logind {
        Some(l) => match l.next().await {
            Some(ev) => ev,
            None => {
                tracing::warn!("logind connection gone");
                *logind = None;
                std::future::pending().await
            }
        },
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

/// `input:repeat_delay` in ms, over IPC. `None` when it cannot be read.
pub async fn repeat_delay(ipc: &Option<Instance>) -> Option<u32> {
    let ipc = ipc.clone()?;
    let v = tokio::task::spawn_blocking(move || ipc.get_option("input:repeat_delay"))
        .await
        .ok()?
        .ok()?;
    v.int
        .or(v.float.map(|f| f as i64))
        .and_then(|i| u32::try_from(i).ok())
}

/// `ecosystem:enforce_permissions` (FR-SAFE-16: with it on, input capture needs a permission).
pub async fn enforce_permissions(ipc: &Option<Instance>) -> Option<bool> {
    let ipc = ipc.clone()?;
    let v = tokio::task::spawn_blocking(move || ipc.get_option("ecosystem:enforce_permissions"))
        .await
        .ok()?
        .ok()?;
    v.boolean.or(v.int.map(|i| i != 0))
}

/// The repeat filter holds a release once a key has been down `repeat_delay − 50` ms,
/// Saturating at 0.
pub fn repeat_hold_after(repeat_delay_ms: u32) -> u32 {
    repeat_delay_ms.saturating_sub(50)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Sessions whose first one never answers a `Release`, nor its `Close`: a hung xdph.
    #[derive(Default)]
    struct Hung {
        opened: Arc<AtomicUsize>,
        log: Arc<Mutex<Vec<(usize, PortalCmd)>>>,
    }

    struct HungSession {
        n: usize,
        log: Arc<Mutex<Vec<(usize, PortalCmd)>>>,
    }

    impl Session for HungSession {
        fn path(&self) -> &str {
            "/test/session"
        }
        async fn execute(&mut self, cmd: &PortalCmd) -> Result<Option<SourceEvent>, PortalError> {
            self.log.lock().unwrap().push((self.n, cmd.clone()));
            if self.n == 1 && matches!(cmd, PortalCmd::Release { .. }) {
                std::future::pending::<()>().await;
            }
            Ok(None)
        }
        async fn next_signal(&mut self) -> PortalSignal {
            std::future::pending().await
        }
        async fn close(self) {
            if self.n == 1 {
                std::future::pending::<()>().await;
            }
        }
    }

    impl Backend for Hung {
        type Session = HungSession;
        async fn open(&mut self) -> Result<(HungSession, OwnedFd), PortalError> {
            let n = self.opened.fetch_add(1, Ordering::Relaxed) + 1;
            let (eis, _) = std::os::unix::net::UnixStream::pair().unwrap();
            let log = self.log.clone();
            Ok((HungSession { n, log }, OwnedFd::from(eis)))
        }
        async fn logind(&mut self) -> Option<Logind> {
            None
        }
    }

    /// A portal call that never answers is abandoned after 1 s, the session is
    /// re-created (its hung `Close` abandoned too), and the messages behind it run on the new
    /// one.
    #[test]
    fn a_portal_call_that_never_answers_recreates_the_session() {
        let backend = Hung::default();
        let (opened, log) = (backend.opened.clone(), backend.log.clone());
        let (capture, from_driver) = calloop::channel::sync_channel(64);
        let live = SharedLive::default();
        let driver = Driver::with_backend(
            backend,
            None,
            Duration::from_millis(50),
            capture,
            live.clone(),
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        let took = local.block_on(&rt, async move {
            let (tx, rx) = mpsc::channel(crate::chan::DRIVER_CAP);
            tokio::task::spawn_local(driver.run(rx));
            let t0 = std::time::Instant::now();
            let release = PortalCmd::Release {
                activation_id: Some(7),
                pos: None,
            };
            tx.send(DriverMsg::Portal(release)).await.unwrap();
            tx.send(DriverMsg::Portal(PortalCmd::Enable)).await.unwrap();
            let (ack, acked) = oneshot::channel();
            tx.send(DriverMsg::Flush(ack)).await.unwrap();
            tokio::time::timeout(Duration::from_secs(4), acked)
                .await
                .expect("the driver moves on")
                .unwrap();
            t0.elapsed()
        });
        // the Release's timeout, then the old session's Close's
        assert!(
            took >= PORTAL_CALL_TIMEOUT && took < 3 * PORTAL_CALL_TIMEOUT,
            "{took:?}"
        );
        assert_eq!(opened.load(Ordering::Relaxed), 2);
        let ran: Vec<(usize, PortalCmd)> = log.lock().unwrap().clone();
        assert!(matches!(ran[0], (1, PortalCmd::Release { .. })), "{ran:?}");
        assert_eq!(ran[1], (2, PortalCmd::Enable));
        assert_eq!(live.lock().counters.get("portal_timeouts"), Some(&1));
        let mut to_capture = Vec::new();
        while let Ok(cmd) = from_driver.try_recv() {
            to_capture.push(cmd);
        }
        assert!(
            matches!(
                to_capture.as_slice(),
                [
                    CaptureCmd::Eis { generation: 1, .. },
                    CaptureCmd::Eis { generation: 2, .. },
                    CaptureCmd::Event(SourceEvent::SessionRestarted),
                ]
            ),
            "{to_capture:?}"
        );
    }

    #[test]
    fn repeat_hold_saturates() {
        assert_eq!(repeat_hold_after(600), 550);
        assert_eq!(repeat_hold_after(40), 0);
        assert_eq!(repeat_hold_after(0), 0);
    }
}
