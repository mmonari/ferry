//! Time as a value. The caller reads `CLOCK_MONOTONIC` and passes it in.

/// Microseconds on the caller's monotonic clock (`CLOCK_MONOTONIC`, the same
/// clock libei stamps EIS events with).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Micros(pub u64);

impl Micros {
    pub const ZERO: Micros = Micros(0);

    pub const fn from_millis(ms: u64) -> Self {
        Micros(ms * 1000)
    }

    /// `self + d`, saturating.
    pub const fn plus(self, d: Micros) -> Self {
        Micros(self.0.saturating_add(d.0))
    }

    /// `self - earlier`, saturating at zero.
    pub const fn since(self, earlier: Micros) -> Micros {
        Micros(self.0.saturating_sub(earlier.0))
    }

    /// Low 32 bits, as carried in a datagram's `t_us` (jitter stats only).
    pub const fn low32(self) -> u32 {
        self.0 as u32
    }
}

/// Held-set snapshot period while a crossing is active.
pub const SNAPSHOT_PERIOD: Micros = Micros::from_millis(100);
/// Target watchdog: release 650 ms after the last snapshot.
pub const TARGET_WATCHDOG: Micros = Micros::from_millis(650);
/// Source liveness: release 650 ms after the send time of the newest
/// acknowledged frame (the S10 one-way-cut fix).
pub const SOURCE_LIVENESS: Micros = Micros::from_millis(650);
/// A crossing sends no motion datagram once its newest `HeldAck` arrived longer ago than
/// this: two snapshot periods (A-04). A live link acknowledges every snapshot period,
/// or every round trip when that is longer. Motion sent into a dead link stays in flight and
/// fills ADR 0019's 64 KiB window floor ahead of the stall probes; the next `Held` or position
/// fence carries the position instead (RG-SAFE-02: motion is latest-wins).
pub const MOTION_ACK_WINDOW: Micros = Micros(2 * SNAPSHOT_PERIOD.0);
/// At most one motion datagram per this (A-04). Until the window above runs out, a
/// 2-8 kHz mouse at one datagram per event puts 1,300-5,200 packets into a 650 ms stall, and
/// the floor holds about 1,100 (`ferry-net/tests/stall.rs`, `rate_matrix`); 1 kHz is what ADR
/// 0019 sized it for.
pub const MOTION_MIN_GAP: Micros = Micros(1000);
/// Arming gives up after this long without a crossing.
pub const ARM_TIMEOUT: Micros = Micros::from_millis(500);
/// Re-zone waits this long after the last `ZonesChanged`/`Disabled`.
pub const REZONE_SETTLE: Micros = Micros::from_millis(100);
/// A failed barrier set is tried again this much later (S11's re-zone retry).
pub const BARRIER_RETRY_DELAY: Micros = Micros::from_millis(1000);
/// ... and at most this many times in a row; then the gate stays `BarriersFailed`
/// until a topology change re-arms it (A-12).
pub const BARRIER_RETRY_MAX: u8 = 5;
/// How long after Pending ended its late `Activated` is still refused. On the
/// netbook the lock check alone took up to 50 ms (row 11, 2026-10-19).
pub const REFUSE_LATE_ACTIVATION: Micros = Micros::from_millis(1000);
/// A lock that starts during a capture is read within this: one active poll
/// interval plus one lock check. D7 allows 350 ms from the compositor's `LOCK`
/// to the release; the other 50 ms are the release itself.
pub const LOCK_SEEN_WITHIN: Micros = Micros::from_millis(300);
/// Pending outlasts the lock check by this much: `Activated` lags EIS
/// `start_emulating` by 0.5–1.4 ms and reaches the machine only after
/// the check, so Pending must not give up while a slow check still runs.
pub const PENDING_MARGIN: Micros = Micros::from_millis(50);
/// Lock poll while local and unlocked.
pub const LOCK_POLL_IDLE: Micros = Micros::from_millis(2000);
/// Lock poll while locked, so crossing returns soon after unlock.
pub const LOCK_POLL_LOCKED: Micros = Micros::from_millis(500);
/// Synthesized-repeat pair: release and press of one key at most this far
/// apart in one EIS read batch.
pub const REPEAT_PAIR_MAX_GAP: Micros = Micros(1000);
/// While a barrier is parked at a wall stretch, the source reads its
/// own cursor this often, to put the barrier back once the cursor leaves.
pub const PARK_POLL: Micros = Micros::from_millis(40);
/// A park ends after this long whatever the cursor reads say, so a cursor
/// left resting by a dead corner does not keep the poll going.
pub const PARK_MAX: Micros = Micros::from_millis(2000);
/// Takeover: while entered, the target reads its own
/// cursor this often.
pub const CURSOR_POLL: Micros = Micros::from_millis(40);
/// A suspect cursor read is read again this much later.
pub const CURSOR_RECHECK: Micros = Micros::from_millis(16);
/// A read is not suspect while injected motion may still be in flight.
pub const TAKEOVER_MOTION_QUIET: Micros = Micros::from_millis(30);
/// Nor this soon after an injected key or button: Hyprland warps the cursor
/// on a focus change made by a bind (`cursor:no_warps` is false by default).
pub const TAKEOVER_KEY_QUIET: Micros = Micros::from_millis(250);

/// The lock check's budget (`[safety] lock_check_budget_ms`): a
/// read of Hyprland's monitors that takes longer counts as unknown. The two
/// timers that depend on it are derived here, so every bound holds whatever
/// the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockBudget(Micros);

impl LockBudget {
    pub const MIN_MS: u32 = 50;
    pub const MAX_MS: u32 = 100;
    /// 50 ms: the budget before it was a setting.
    pub const DEFAULT: LockBudget = LockBudget(Micros::from_millis(Self::MIN_MS as u64));

    /// `ms` clamped into `MIN_MS..=MAX_MS`, and whether it had to be.
    pub fn clamped(ms: u32) -> (Self, bool) {
        let c = ms.clamp(Self::MIN_MS, Self::MAX_MS);
        (LockBudget(Micros::from_millis(u64::from(c))), c != ms)
    }

    pub fn as_micros(self) -> Micros {
        self.0
    }

    pub fn as_duration(self) -> std::time::Duration {
        std::time::Duration::from_micros(self.0.0)
    }

    pub fn millis(self) -> u32 {
        (self.0.0 / 1000) as u32
    }

    /// Lock poll while a crossing is pending, arming or remote: poll + budget
    /// = [`LOCK_SEEN_WITHIN`]. 250 ms at the default 50, 200 ms at 100.
    pub fn active_poll(self) -> Micros {
        LOCK_SEEN_WITHIN.since(self.0)
    }

    /// Pending gives up when the portal's `Activated` has not arrived: budget
    /// + [`PENDING_MARGIN`]. 100 ms at the default 50, 150 ms at 100.
    pub fn pending_timeout(self) -> Micros {
        self.0.plus(PENDING_MARGIN)
    }
}

impl Default for LockBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_budget_keeps_the_old_timers() {
        let b = LockBudget::default();
        assert_eq!(b.millis(), 50);
        assert_eq!(b.active_poll(), Micros::from_millis(250));
        assert_eq!(b.pending_timeout(), Micros::from_millis(100));
        assert_eq!(b.as_duration(), std::time::Duration::from_millis(50));
    }

    #[test]
    fn every_budget_keeps_both_bounds() {
        for ms in LockBudget::MIN_MS..=LockBudget::MAX_MS {
            let (b, clamped) = LockBudget::clamped(ms);
            assert!(!clamped);
            // A lock is read within poll + check = 300 ms, 50 ms before 350.
            assert_eq!(b.active_poll().plus(b.as_micros()), LOCK_SEEN_WITHIN);
            // Pending outlasts a check that uses the whole budget, by 50 ms.
            assert_eq!(b.pending_timeout().since(b.as_micros()), PENDING_MARGIN);
        }
        let b = LockBudget::clamped(100).0;
        assert_eq!(b.active_poll(), Micros::from_millis(200));
        assert_eq!(b.pending_timeout(), Micros::from_millis(150));
    }

    #[test]
    fn a_budget_out_of_range_is_clamped() {
        assert_eq!(LockBudget::clamped(0), (LockBudget::DEFAULT, true));
        assert_eq!(LockBudget::clamped(49).0.millis(), 50);
        assert_eq!(LockBudget::clamped(101), (LockBudget::clamped(100).0, true));
        assert_eq!(LockBudget::clamped(u32::MAX).0.millis(), 100);
    }
}
