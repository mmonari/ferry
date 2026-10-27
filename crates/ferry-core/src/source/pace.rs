//! What a crossing puts into the link for motion (A-04). EIS gives one `Motion` per
//! pointer event, so an 8 kHz mouse would send 8,000 datagrams a second, and into a dead link
//! they fill ADR 0019's window floor ahead of the stall probes. Motion is latest-wins:
//! A datagram not sent is carried by the next one, by the next `Held`, or by
//! the position fence of the next key, button or scroll frame. Keys and buttons never pass
//! through here.

use crate::time::{MOTION_ACK_WINDOW, MOTION_MIN_GAP, Micros};

/// What becomes of one motion the cursor model took.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pace {
    /// Send a datagram now. `dx, dy` sum every motion it carries.
    Send { dx: f64, dy: f64 },
    /// Within [`MOTION_MIN_GAP`] of the last datagram: it leaves at
    /// [`MotionPacer::deadline`], with whatever follows it.
    Coalesced,
    /// No `HeldAck` within [`MOTION_ACK_WINDOW`]: not sent at all.
    Suppressed,
}

/// The motion pacing of one crossing: at most one datagram per [`MOTION_MIN_GAP`], none once
/// the newest `HeldAck` is older than [`MOTION_ACK_WINDOW`].
///
/// The ack's arrival time counts, not the send time of the `Held` it answers (the liveness
/// clock, `SOURCE_LIVENESS`): measured from the send time, a live link with a round trip over
/// 100 ms would lose its motion every snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionPacer {
    ack_heard: Micros,
    sent_at: Option<Micros>,
    /// Coalesced motion no datagram has carried yet (its deltas, summed).
    owed: Option<(f64, f64)>,
}

impl MotionPacer {
    /// A crossing that starts `now` counts as acknowledged `now`: its `Enter` and first `Held`
    /// are on their way.
    pub fn new(now: Micros) -> Self {
        Self {
            ack_heard: now,
            sent_at: None,
            owed: None,
        }
    }

    /// A `HeldAck` arrived.
    pub fn acked(&mut self, now: Micros) {
        self.ack_heard = self.ack_heard.max(now);
    }

    /// The newest `HeldAck` is older than [`MOTION_ACK_WINDOW`].
    pub fn stale(&self, now: Micros) -> bool {
        now.since(self.ack_heard) > MOTION_ACK_WINDOW
    }

    /// One motion of `dx, dy` the model took `now`.
    pub fn motion(&mut self, dx: f64, dy: f64, now: Micros) -> Pace {
        if self.stale(now) {
            self.owed = None;
            return Pace::Suppressed;
        }
        let (ox, oy) = self.owed.take().unwrap_or_default();
        let (dx, dy) = (dx + ox, dy + oy);
        if self.sent_at.is_some_and(|t| now.since(t) < MOTION_MIN_GAP) {
            self.owed = Some((dx, dy));
            return Pace::Coalesced;
        }
        self.sent_at = Some(now);
        Pace::Send { dx, dy }
    }

    /// When the coalesced motion is due, if there is any.
    pub fn deadline(&self) -> Option<Micros> {
        self.owed?;
        self.sent_at.map(|t| t.plus(MOTION_MIN_GAP))
    }

    /// The coalesced motion's datagram, once it is due. Into a stale link it is dropped
    /// instead: the `Held` that leaves with the next ack carries the position.
    pub fn flush(&mut self, now: Micros) -> Option<(f64, f64)> {
        if self.deadline().is_none_or(|d| now < d) {
            return None;
        }
        let owed = self.owed.take();
        if self.stale(now) {
            return None;
        }
        self.sent_at = Some(now);
        owed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn us(n: u64) -> Micros {
        Micros(n)
    }

    #[test]
    fn eight_events_a_millisecond_leave_as_one_datagram_with_their_sum() {
        let mut p = MotionPacer::new(us(0));
        assert_eq!(p.motion(1.0, 0.0, us(0)), Pace::Send { dx: 1.0, dy: 0.0 });
        for i in 1..8 {
            assert_eq!(p.motion(1.0, 2.0, us(i * 125)), Pace::Coalesced);
        }
        assert_eq!(p.deadline(), Some(us(1000)));
        assert_eq!(p.flush(us(999)), None);
        assert_eq!(p.flush(us(1000)), Some((7.0, 14.0)));
        assert_eq!(p.deadline(), None);
        // The next one is within the gap of the flush.
        assert_eq!(p.motion(1.0, 0.0, us(1100)), Pace::Coalesced);
        assert_eq!(
            p.motion(1.0, 0.0, us(2000)),
            Pace::Send { dx: 2.0, dy: 0.0 }
        );
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn a_1_khz_mouse_is_not_held() {
        let mut p = MotionPacer::new(us(0));
        for i in 0..100 {
            assert!(matches!(
                p.motion(1.0, 0.0, us(i * 1000)),
                Pace::Send { .. }
            ));
            if i % 10 == 9 {
                p.acked(us(i * 1000));
            }
        }
    }

    #[test]
    fn no_motion_once_the_newest_ack_is_older_than_two_snapshot_periods() {
        let mut p = MotionPacer::new(us(0));
        p.acked(us(50_000));
        let edge = us(50_000).plus(MOTION_ACK_WINDOW);
        assert!(matches!(p.motion(1.0, 0.0, edge), Pace::Send { .. }));
        assert_eq!(p.motion(1.0, 0.0, edge.plus(us(1))), Pace::Suppressed);
        assert_eq!(p.motion(1.0, 0.0, edge.plus(us(5000))), Pace::Suppressed);
        // Nothing owed, so no timer while the link is dead.
        assert_eq!(p.deadline(), None);
        // The ack is back: motion leaves at once.
        p.acked(us(900_000));
        assert_eq!(
            p.motion(3.0, 4.0, us(900_100)),
            Pace::Send { dx: 3.0, dy: 4.0 }
        );
    }

    #[test]
    fn coalesced_motion_due_after_the_ack_window_is_dropped_not_spun_on() {
        let mut p = MotionPacer::new(us(0));
        let last = MOTION_ACK_WINDOW.since(us(400));
        assert!(matches!(p.motion(1.0, 0.0, last), Pace::Send { .. }));
        assert_eq!(p.motion(1.0, 0.0, last.plus(us(300))), Pace::Coalesced);
        let due = p.deadline().unwrap();
        assert!(p.stale(due));
        assert_eq!(p.flush(due), None);
        assert_eq!(p.deadline(), None, "nothing left to wake up for");
    }

    #[test]
    fn a_late_ack_does_not_move_the_clock_back() {
        let mut p = MotionPacer::new(us(100_000));
        p.acked(us(10));
        assert!(!p.stale(us(100_000).plus(MOTION_ACK_WINDOW)));
    }
}
