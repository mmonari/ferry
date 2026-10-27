//! What the machines publish for `ferry status`.
//!
//! The capture and net threads own the machines; after each wake-up they copy the machine's
//! status and counters here, and record a release when a crossing ends. `ctl` reads it when it
//! answers `status`. One uncontended mutex: nothing on a hot path waits for `ctl`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::watchdog::Beats;

use ferry_core::source::{Gate, SourceCounters, SourceRole, SourceStatus};
use ferry_core::target::{TargetCounters, TargetStatus};
use ferry_core::wire::LeaveReason;

#[derive(Debug, Default)]
pub struct Live {
    /// `None` while capture is off on this peer (`input.capture = "off"`).
    pub source: Option<SourceStatus>,
    pub source_counters: SourceCounters,
    pub target: Option<TargetStatus>,
    pub target_counters: TargetCounters,
    /// The barriers of the last `SetBarriers` ctl ran, and the ids the portal failed.
    pub barriers: u32,
    pub failed_barriers: Vec<u32>,
    /// The last lock check: `None` while unknown.
    pub screen_locked: Option<bool>,
    /// The newest end of a crossing on either side.
    pub last_release: Option<(LeaveReason, Instant)>,
    /// Counters of the threads themselves (frames dropped, unknown variants, …).
    pub counters: BTreeMap<&'static str, u64>,
    /// ferry-inject is running (this peer can be a target).
    pub inject: bool,
    /// Up peers whose `PeerState` says they are locked to themselves (`net` keeps it).
    pub locked_peers: Vec<ferry_core::PeerId>,
    /// The portal session, for `ferry doctor`.
    pub portal: PortalHealth,
    /// `ecosystem:enforce_permissions`, read at start and whenever Hyprland reloads its config.
    pub enforce_permissions: Option<bool>,
    /// When the capture gate entered `rezoning`; `None` while it is not there.
    pub rezoning_since: Option<Instant>,
    /// The systemd service this ferryd runs in, read at start when `sd_notify` is on.
    pub service: Option<Service>,
    /// `ctl` began the design §9 shutdown: the capture thread stops first, so a closed channel
    /// to it is the shutdown order from then on, not a fault.
    pub shutting_down: bool,
}

/// The systemd service unit ferryd runs in, for `ferry doctor`'s `unit` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// The unit, from this process's cgroup: `ferryd.service`, or a test unit such as
    /// `ferryd-m1exit.service` from `systemd-run`.
    pub name: String,
    /// systemd's watchdog interval for this process (`WATCHDOG_USEC`), when it set one.
    pub watchdog: Option<std::time::Duration>,
}

/// What the portal driver knows about the session it keeps.
#[derive(Debug, Default, Clone)]
pub struct PortalHealth {
    /// A session is open.
    pub open: bool,
    /// Failed attempts since the last open one.
    pub failures: u32,
    /// The last open failure, as the portal reported it.
    pub last_error: Option<String>,
}

/// What wakes the event hub (`events.rs`) from any thread: a thread that changed something
/// `status` shows says so with [`SharedLive::changed`]. It costs one flag read until a frontend
/// subscribes, so a daemon nobody watches pays nothing.
///
/// The toasts (`notify.rs`) listen too, whether or not a frontend is subscribed: a new
/// `attention` entry is a toast, and it can come with nobody watching. That listener is the
/// notify task alone, and it is switched on once, when the task starts.
#[derive(Debug, Default)]
pub struct Changed {
    watching: AtomicBool,
    wake: tokio::sync::Notify,
    toasting: AtomicBool,
    toast: tokio::sync::Notify,
}

impl Changed {
    /// Something a subscriber may see changed. Wakes the hub when there is one to wake, and the
    /// notify task when it listens.
    pub fn changed(&self) {
        // A flag, not a publication of data: whoever sets it does so before it computes the
        // snapshot it sends, so a change either shows in that snapshot or wakes the hub.
        if self.watching.load(Ordering::Relaxed) {
            self.wake.notify_one();
        }
        self.toast_wake();
    }

    /// The notify task listens (`true`, once). Until then a change costs this flag read.
    pub fn toast_on(&self) {
        self.toasting.store(true, Ordering::Relaxed);
    }

    /// Wakes the notify task, when it listens: `status.attention` may have changed. `ctl` calls
    /// it where it pokes the hub.
    pub fn toast_wake(&self) {
        if self.toasting.load(Ordering::Relaxed) {
            self.toast.notify_one();
        }
    }

    /// Resolves after [`toast_wake`](Self::toast_wake) since the last time it resolved.
    pub async fn toast_woken(&self) {
        self.toast.notified().await;
    }

    /// The hub has subscribers (`true`) or has none (`false`).
    pub fn watch(&self, on: bool) {
        self.watching.store(on, Ordering::Relaxed);
    }

    /// Wakes the hub, whether or not anyone is watching: ctl's own triggers know they matter.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Resolves after [`wake`](Self::wake) or [`changed`](Self::changed) (while watching) since
    /// the last time it resolved.
    pub async fn woken(&self) {
        self.wake.notified().await;
    }
}

/// Shared between the threads. The heartbeat cells of the daemon watchdog sit beside the mutex,
/// not behind it: a thread that stalls holding the lock must not hide that it stopped beating.
#[derive(Debug, Clone, Default)]
pub struct SharedLive(Arc<Mutex<Live>>, Arc<Beats>, Arc<Changed>);

impl SharedLive {
    /// The watchdog's `last_alive` cells (`watchdog.rs`).
    pub fn beats(&self) -> &Arc<Beats> {
        &self.1
    }

    /// What the event hub waits on.
    pub fn signal(&self) -> &Arc<Changed> {
        &self.2
    }

    /// A thread changed something `status` shows, without a message to ctl: wake the hub.
    pub fn changed(&self) {
        self.2.changed();
    }

    pub fn lock(&self) -> MutexGuard<'_, Live> {
        // A thread that panicked while holding it left plain data behind: keep serving it.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn bump(&self, counter: &'static str) {
        self.add(counter, 1);
    }

    /// Add `n` to a counter; a counter that stays at zero is not created.
    pub fn add(&self, counter: &'static str, n: u64) {
        if n > 0 {
            let mut l = self.lock();
            let c = l.counters.entry(counter).or_default();
            *c = c.saturating_add(n);
        }
    }

    /// The source machine's status changed. A crossing that ends is recorded as a release.
    pub fn source_status(&self, s: SourceStatus) {
        let mut l = self.lock();
        let was_remote = l
            .source
            .as_ref()
            .is_some_and(|p| p.role == SourceRole::Remote);
        if was_remote && s.role != SourceRole::Remote {
            l.last_release = Some((s.last_leave.unwrap_or(LeaveReason::Other), Instant::now()));
        }
        // One re-zone per burst of ZonesChanged/Disabled: the gate enters Rezoning once.
        let was_rezoning = l.source.as_ref().is_some_and(|p| p.gate == Gate::Rezoning);
        if s.gate == Gate::Rezoning && !was_rezoning {
            *l.counters.entry("rezones").or_default() += 1;
            l.rezoning_since = Some(Instant::now());
        }
        if s.gate != Gate::Rezoning {
            l.rezoning_since = None;
        }
        // Once per change of status, not per wake-up: the machines publish after every one.
        let news = l.source.as_ref() != Some(&s);
        l.source = Some(s);
        drop(l);
        if news {
            self.changed();
        }
    }

    /// The target machine's status changed. A session that ends is recorded as a release.
    pub fn target_status(&self, s: TargetStatus) {
        let mut l = self.lock();
        let was_entered = l.target.as_ref().is_some_and(|p| p.entered_by.is_some());
        if was_entered && s.entered_by.is_none() {
            l.last_release = Some((s.last_end.unwrap_or(LeaveReason::Other), Instant::now()));
        }
        let news = l.target.as_ref() != Some(&s);
        l.target = Some(s);
        drop(l);
        if news {
            self.changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_core::PeerId;

    fn src(role: SourceRole, last: Option<LeaveReason>) -> SourceStatus {
        SourceStatus {
            role,
            peer: Some(PeerId([2; 32])),
            session: 1,
            last_leave: last,
            gate: Gate::Armed,
            suspect: Vec::new(),
            controlled: false,
            locked_here: false,
            locked_peers: Vec::new(),
            edge: None,
        }
    }

    #[test]
    fn a_crossing_that_ends_is_a_release() {
        let l = SharedLive::default();
        l.source_status(src(SourceRole::Local, None));
        assert!(l.lock().last_release.is_none());
        l.source_status(src(SourceRole::Remote, None));
        l.source_status(src(SourceRole::Local, Some(LeaveReason::Silence)));
        assert_eq!(l.lock().last_release.unwrap().0, LeaveReason::Silence);

        l.target_status(TargetStatus {
            entered_by: Some(PeerId([3; 32])),
            entry: None,
            session: 4,
            last_end: None,
        });
        l.target_status(TargetStatus {
            entered_by: None,
            entry: None,
            session: 4,
            last_end: Some(LeaveReason::Watchdog),
        });
        assert_eq!(l.lock().last_release.unwrap().0, LeaveReason::Watchdog);
    }

    #[test]
    fn a_rezone_is_counted_once_per_burst() {
        let l = SharedLive::default();
        let with = |gate| SourceStatus {
            gate,
            ..src(SourceRole::Local, None)
        };
        l.source_status(with(Gate::Armed));
        l.source_status(with(Gate::Rezoning));
        l.source_status(with(Gate::Rezoning));
        l.source_status(with(Gate::Armed));
        l.source_status(with(Gate::Rezoning));
        assert_eq!(l.lock().counters["rezones"], 2);
    }
}
