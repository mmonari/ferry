//! The synthesized-repeat filter. Lifted from
//! `spikes/s10-stuck-keys/src/wire.rs` (`RepeatFilter`).
//!
//! Hyprland turns a held key into `Released` + `Press` pairs of the same key,
//! 10–822 µs apart. S17 saw them inside one EIS read batch, but the M1 VM
//! drills saw 2 of 55 pairs split across two reads (the press 244 and 452 µs
//! later, in the next batch). So a release that may start a pair is held
//! until `t_eis + 1000 µs`, a deadline the source machine arms and feeds with
//! `Tick`, instead of only until the end of the batch.
//!
//! [`HOLD_ACROSS_BATCHES`] switches between the two mechanisms; the
//! threshold `hold_after` limits the hold to keys down long enough to
//! repeat, so ordinary typing gets no added latency.
//!
//! **Split pairs (a diagnostic).** Under VM load a pair's press can come more than 1 ms after
//! its release (the duplex stream's D13 and D9 findings), and the target then gets an extra
//! press. The filter does not change for that: it only counts, in `split_pairs` and
//! `split_max_us`, a release held back for a repeat and followed by a press of the same key
//! more than [`REPEAT_PAIR_MAX_GAP`] but less than [`REPEAT_SPLIT_MAX_GAP`] later, so that the
//! real machines can say whether the gap should widen (the user's decision, open).

use crate::held::MAX_HELD_KEYS;
use crate::time::{Micros, REPEAT_PAIR_MAX_GAP};

/// A press of the same key this close to a held-back release, but past [`REPEAT_PAIR_MAX_GAP`],
/// is a split pair: counted, never filtered.
pub const REPEAT_SPLIT_MAX_GAP: Micros = Micros(10_000);

/// `true`: a held-back release waits for its deadline (`t_eis + 1000 µs`),
/// across EIS read batches. `false`: the M0 D7 mechanism, where the end of
/// the batch forwards it (pairs split across reads leak).
pub const HOLD_ACROSS_BATCHES: bool = true;

/// A release is held back until the next key event, its deadline (or, with
/// [`HOLD_ACROSS_BATCHES`] off, the end of the batch); a press of the same
/// key within [`REPEAT_PAIR_MAX_GAP`] of it cancels both. Only releases of
/// keys that were down for at least `hold_after` are held; `hold_after = 0`
/// holds every release (safe, at most 1 ms on any release).
#[derive(Debug, Default, Clone)]
pub struct RepeatFilter {
    /// The release held back: (evdev code, EIS time, deadline). The deadline is measured from
    /// the earlier of the EIS time and the handling time, so a release stamped ahead of this
    /// process's clock still goes out 1 ms after it arrived (A-08).
    pending: Option<(u16, Micros, Micros)>,
    /// Keys forwarded as down, with the EIS time they went down.
    down: [(u16, Micros); MAX_HELD_KEYS],
    down_len: usize,
    hold_after: Micros,
    /// Pairs dropped since start (a counter for `ferry status`).
    pub dropped_pairs: u64,
    /// The release of a key that was past `hold_after` and went out because nothing came within
    /// 1 ms: (evdev code, its EIS time). The next key edge may still be its press.
    released: Option<(u16, Micros)>,
    /// Release-then-press pairs of one held key with a gap over 1 ms and under 10 ms. Forwarded
    /// as two edges, as ever; only counted.
    pub split_pairs: u64,
    /// The largest such gap, in µs.
    pub split_max_us: u64,
    /// Key edges stamped ahead of the handling time (a compositor on another clock, or a bad
    /// timestamp). Their holds are bounded by the handling time.
    pub future_stamps: u64,
}

impl RepeatFilter {
    /// A filter that holds releases of keys down for at least `hold_after`
    /// (ctl: the source's `input:repeat_delay` − 50 ms; 0 = every release).
    pub fn with_hold_after(hold_after: Micros) -> Self {
        Self {
            hold_after,
            ..Self::default()
        }
    }

    pub fn set_hold_after(&mut self, hold_after: Micros) {
        self.hold_after = hold_after;
    }

    /// Feed one key edge with its EIS timestamp and the handling time `now` (the clock of
    /// [`Self::expire`]); `emit(code, pressed)` is called for each edge to forward, in order
    /// (0, 1 or 2 calls).
    pub fn key(
        &mut self,
        code: u16,
        pressed: bool,
        t_eis: Micros,
        now: Micros,
        mut emit: impl FnMut(u16, bool),
    ) {
        if t_eis > now {
            self.future_stamps += 1;
        }
        let released = self.released.take();
        if let Some((held_code, held_at, _)) = self.pending.take() {
            if pressed && held_code == code && t_eis.since(held_at) <= REPEAT_PAIR_MAX_GAP {
                // Still held: the key stays down since its first press.
                self.dropped_pairs += 1;
                return;
            }
            if pressed && held_code == code {
                self.split(t_eis.since(held_at));
            }
            self.forget(held_code);
            emit(held_code, false);
        } else if let Some((code_before, at)) = released
            && pressed
            && code_before == code
        {
            self.split(t_eis.since(at));
        }
        if pressed {
            self.remember(code, t_eis);
            emit(code, true);
        } else if self.may_repeat(code, t_eis) {
            self.pending = Some((code, t_eis, t_eis.min(now).plus(REPEAT_PAIR_MAX_GAP)));
        } else {
            self.forget(code);
            emit(code, false);
        }
    }

    /// The EIS read batch is drained. With [`HOLD_ACROSS_BATCHES`] the
    /// held-back release waits for its deadline instead, so this returns
    /// `None`; otherwise the held-back release is real.
    pub fn batch_end(&mut self) -> Option<u16> {
        if HOLD_ACROSS_BATCHES {
            return None;
        }
        self.flush()
    }

    /// When the held-back release must be forwarded: `min(t_eis, now_at_key) + 1000 µs`.
    pub fn deadline(&self) -> Option<Micros> {
        if !HOLD_ACROSS_BATCHES {
            return None;
        }
        self.pending.map(|(_, _, d)| d)
    }

    /// `now` (the same clock as EIS times) reached the deadline: the
    /// held-back release is real.
    pub fn expire(&mut self, now: Micros) -> Option<u16> {
        if self.deadline().is_some_and(|d| now >= d) {
            self.flush()
        } else {
            None
        }
    }

    /// Forget a held-back release and every down time (on any exit from a
    /// crossing).
    pub fn reset(&mut self) {
        self.pending = None;
        self.released = None;
        self.down_len = 0;
    }

    /// Counts a press that came `gap` after a held-back release, if it is a split pair.
    fn split(&mut self, gap: Micros) {
        if gap > REPEAT_PAIR_MAX_GAP && gap < REPEAT_SPLIT_MAX_GAP {
            self.split_pairs += 1;
            self.split_max_us = self.split_max_us.max(gap.0);
        }
    }

    fn flush(&mut self) -> Option<u16> {
        let (code, at, _) = self.pending.take()?;
        self.forget(code);
        self.released = Some((code, at));
        Some(code)
    }

    /// A release may start a synthesized pair when the key has been down
    /// for at least `hold_after`. A key never seen going down (pressed
    /// before the crossing) is held, the safe side.
    fn may_repeat(&self, code: u16, t: Micros) -> bool {
        match self.down[..self.down_len].iter().find(|(c, _)| *c == code) {
            Some(&(_, since)) => t.since(since) >= self.hold_after,
            None => true,
        }
    }

    fn remember(&mut self, code: u16, t: Micros) {
        let known = self.down[..self.down_len].iter().any(|(c, _)| *c == code);
        if !known && let Some(slot) = self.down.get_mut(self.down_len) {
            *slot = (code, t);
            self.down_len += 1;
        }
    }

    fn forget(&mut self, code: u16) {
        if let Some(i) = self.down[..self.down_len]
            .iter()
            .position(|(c, _)| *c == code)
        {
            self.down.copy_within(i + 1..self.down_len, i);
            self.down_len -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn feed(f: &mut RepeatFilter, code: u16, pressed: bool, t: u64) -> Vec<(u16, bool)> {
        let mut out = Vec::new();
        f.key(code, pressed, Micros(t), Micros(t), |c, p| out.push((c, p)));
        out
    }

    #[test]
    fn pair_dropped() {
        // The S10 `repeat_pairs_are_dropped_real_taps_pass` values.
        let mut f = RepeatFilter::default();
        assert_eq!(feed(&mut f, 30, true, 1_000), vec![(30, true)]);
        // A synthesized pair 15 µs apart.
        assert!(feed(&mut f, 30, false, 251_000).is_empty());
        assert!(feed(&mut f, 30, true, 251_015).is_empty());
        assert_eq!(f.dropped_pairs, 1);
        // A real release, then a different key: the release goes out first.
        assert!(feed(&mut f, 30, false, 400_000).is_empty());
        assert_eq!(
            feed(&mut f, 31, true, 400_100),
            vec![(30, false), (31, true)]
        );
        // A real release followed by a re-press 20 ms later.
        assert!(feed(&mut f, 31, false, 500_000).is_empty());
        assert_eq!(
            feed(&mut f, 31, true, 520_000),
            vec![(31, false), (31, true)]
        );
        // A release with nothing after it: forwarded at its deadline.
        assert!(feed(&mut f, 31, false, 600_000).is_empty());
        assert_eq!(f.deadline(), Some(Micros(601_000)));
        assert_eq!(f.expire(Micros(600_999)), None);
        assert_eq!(f.expire(Micros(601_000)), Some(31));
        assert_eq!(f.deadline(), None);
        // The 1000 µs boundary is inclusive; 1001 is a real re-press.
        assert!(feed(&mut f, 32, false, 700_000).is_empty());
        assert!(feed(&mut f, 32, true, 701_000).is_empty());
        assert!(feed(&mut f, 32, false, 800_000).is_empty());
        assert_eq!(
            feed(&mut f, 32, true, 801_001),
            vec![(32, false), (32, true)]
        );
        assert_eq!(f.dropped_pairs, 2);
        // reset() forgets a held-back release.
        assert!(feed(&mut f, 33, false, 900_000).is_empty());
        f.reset();
        assert_eq!(f.deadline(), None);
    }

    #[test]
    fn pair_split_across_batches() {
        // The VM a drill: the press 244 and 452 µs after the release, in
        // the next EIS read.
        for gap in [244, 452] {
            let mut f = RepeatFilter::default();
            assert_eq!(feed(&mut f, 30, true, 0), vec![(30, true)]);
            assert!(feed(&mut f, 30, false, 600_000).is_empty());
            assert_eq!(f.batch_end(), None);
            assert!(feed(&mut f, 30, true, 600_000 + gap).is_empty());
            assert_eq!(f.dropped_pairs, 1);
            assert_eq!(f.deadline(), None);
        }
    }

    #[test]
    fn short_holds_are_never_delayed() {
        // ctl: repeat_delay 600 − 50 = 550 ms.
        let mut f = RepeatFilter::with_hold_after(Micros::from_millis(550));
        // A 100 ms tap: the release goes out at once.
        assert_eq!(feed(&mut f, 30, true, 0), vec![(30, true)]);
        assert_eq!(feed(&mut f, 30, false, 100_000), vec![(30, false)]);
        assert_eq!(f.deadline(), None);
        // A 600 ms hold: its release may start a pair, so it is held.
        assert_eq!(feed(&mut f, 31, true, 200_000), vec![(31, true)]);
        assert!(feed(&mut f, 31, false, 800_000).is_empty());
        assert!(feed(&mut f, 31, true, 800_015).is_empty());
        // The key stays down since its first press: the next pair is held
        // too, and dropped.
        assert!(feed(&mut f, 31, false, 833_000).is_empty());
        assert!(feed(&mut f, 31, true, 833_020).is_empty());
        assert_eq!(f.dropped_pairs, 2);
        // A release of a key never seen going down is held (safe side).
        assert!(feed(&mut f, 99, false, 900_000).is_empty());
        assert_eq!(f.expire(Micros(901_000)), Some(99));
    }

    #[test]
    fn a_future_stamped_release_is_forwarded_within_1_ms() {
        // A-08: the release is stamped 10 s ahead of this process's clock. The hold is bounded
        // by the handling time, so it goes out 1 ms after it arrived, not 10 s.
        let mut f = RepeatFilter::default();
        assert_eq!(feed(&mut f, 30, true, 0), vec![(30, true)]);
        let now = Micros(600_000);
        let mut out = Vec::new();
        f.key(30, false, now.plus(Micros(10_000_000)), now, |c, p| {
            out.push((c, p))
        });
        assert!(out.is_empty(), "held back, as for any release");
        assert_eq!(f.deadline(), Some(now.plus(REPEAT_PAIR_MAX_GAP)));
        assert_eq!(f.expire(now.plus(Micros(999))), None);
        assert_eq!(f.expire(now.plus(REPEAT_PAIR_MAX_GAP)), Some(30));
        assert_eq!(f.future_stamps, 1);

        // an on-time stamp is unchanged and not counted
        assert_eq!(feed(&mut f, 31, true, 700_000), vec![(31, true)]);
        assert!(feed(&mut f, 31, false, 800_000).is_empty());
        assert_eq!(f.deadline(), Some(Micros(801_000)));
        assert_eq!(f.future_stamps, 1);
    }

    #[test]
    fn split_pairs_are_counted_and_still_forwarded() {
        // Behaviour first: a press 2.5 ms after a held release is not a dropped pair. Both edges
        // go out, as before this counter existed; only the counter moves.
        let mut f = RepeatFilter::default();
        assert_eq!(feed(&mut f, 30, true, 0), vec![(30, true)]);
        assert!(feed(&mut f, 30, false, 600_000).is_empty());
        assert_eq!(
            feed(&mut f, 30, true, 602_500),
            vec![(30, false), (30, true)],
            "a split pair is not filtered"
        );
        assert_eq!(
            (f.dropped_pairs, f.split_pairs, f.split_max_us),
            (0, 1, 2_500)
        );

        // The same, when the deadline fired first (the machine's timer ran before the next read).
        assert!(feed(&mut f, 30, false, 700_000).is_empty());
        assert_eq!(f.expire(Micros(701_000)), Some(30));
        assert_eq!(feed(&mut f, 30, true, 706_000), vec![(30, true)]);
        assert_eq!(
            (f.dropped_pairs, f.split_pairs, f.split_max_us),
            (0, 2, 6_000)
        );

        // A smaller gap leaves the maximum alone.
        assert!(feed(&mut f, 30, false, 800_000).is_empty());
        assert_eq!(feed(&mut f, 30, true, 801_500).len(), 2);
        assert_eq!((f.split_pairs, f.split_max_us), (3, 6_000));
    }

    #[test]
    fn only_gaps_between_1_and_10_ms_count() {
        let mut gaps = Vec::new();
        for gap in [15u64, 1_000, 1_001, 9_999, 10_000, 40_000] {
            let mut f = RepeatFilter::default();
            feed(&mut f, 30, true, 0);
            assert!(feed(&mut f, 30, false, 600_000).is_empty());
            feed(&mut f, 30, true, 600_000 + gap);
            gaps.push((gap, f.dropped_pairs, f.split_pairs));
        }
        assert_eq!(
            gaps,
            [
                (15, 1, 0),
                (1_000, 1, 0),
                (1_001, 0, 1),
                (9_999, 0, 1),
                (10_000, 0, 0),
                (40_000, 0, 0)
            ]
        );
    }

    #[test]
    fn not_a_repeat_shape_is_not_counted() {
        let mut f = RepeatFilter::with_hold_after(Micros::from_millis(550));
        // A key down for 100 ms (under hold_after) released and pressed 3 ms later: ordinary
        // typing, no release was held back.
        feed(&mut f, 30, true, 0);
        assert_eq!(feed(&mut f, 30, false, 100_000), vec![(30, false)]);
        assert_eq!(feed(&mut f, 30, true, 103_000), vec![(30, true)]);
        assert_eq!((f.dropped_pairs, f.split_pairs), (0, 0));
        let mut f = RepeatFilter::default();
        feed(&mut f, 30, true, 0);
        // Another key's edge between the release and the press ends the pair.
        feed(&mut f, 30, false, 700_000);
        assert_eq!(f.expire(Micros(701_000)), Some(30));
        feed(&mut f, 31, true, 702_000);
        feed(&mut f, 30, true, 704_000);
        // A release that nothing follows, and a reset.
        feed(&mut f, 31, false, 900_000);
        assert_eq!(f.expire(Micros(901_000)), Some(31));
        f.reset();
        feed(&mut f, 31, true, 903_000);
        assert_eq!((f.dropped_pairs, f.split_pairs, f.split_max_us), (0, 0, 0));
    }

    #[derive(Debug, Clone, Copy)]
    enum In {
        Key { code: u16, pressed: bool, dt: u64 },
        BatchEnd,
    }

    fn arb_input() -> impl Strategy<Value = Vec<In>> {
        proptest::collection::vec(
            prop_oneof![
                4 => (0u16..4, any::<bool>(), prop_oneof![0u64..1500, 0u64..50_000, 0u64..400_000])
                    .prop_map(|(code, pressed, dt)| In::Key { code, pressed, dt }),
                2 => Just(In::BatchEnd),
            ],
            0..120,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(8192))]

        /// For any sequence, with releases split from their press by batch
        /// ends and the deadline fired as the caller's timer would: no
        /// release+press of one key ≤ 1 ms apart that may be a synthesized
        /// pair reaches the output, and every other edge does, in order.
        #[test]
        fn never_forwards_a_pair(input in arb_input(), hold_ms in prop_oneof![Just(0u64), 0u64..600]) {
            let hold_after = Micros::from_millis(hold_ms);
            let mut timed = Vec::new();
            let mut t = 0u64;
            for i in &input {
                match *i {
                    In::Key { code, pressed, dt } => {
                        t += dt;
                        timed.push(Some((code, pressed, t)));
                    }
                    In::BatchEnd => timed.push(None),
                }
            }
            // Reference model.
            let mut expected = Vec::new();
            let mut down: Vec<(u16, u64)> = Vec::new();
            let mut i = 0;
            while i < timed.len() {
                let Some((c, p, t0)) = timed[i] else { i += 1; continue };
                if p {
                    if !down.iter().any(|d| d.0 == c) { down.push((c, t0)); }
                    expected.push((c, true));
                    i += 1;
                    continue;
                }
                let since = down.iter().find(|d| d.0 == c).map(|d| d.1);
                let may_repeat = since.is_none_or(|s| t0 - s >= hold_after.0);
                // The next key event, across batch ends unless the
                // mechanism is batch-only.
                let mut j = i + 1;
                let mut cut = false;
                while j < timed.len() && timed[j].is_none() {
                    cut |= !HOLD_ACROSS_BATCHES;
                    j += 1;
                }
                if may_repeat && !cut
                    && let Some(Some((c2, true, t1))) = timed.get(j).copied()
                    && c2 == c && t1 - t0 <= 1000
                {
                    i = j + 1;
                    continue;
                }
                down.retain(|d| d.0 != c);
                expected.push((c, false));
                i += 1;
            }
            // The filter, with the deadline fired before any event that comes
            // after it.
            let mut f = RepeatFilter::with_hold_after(hold_after);
            let mut out = Vec::new();
            for e in timed {
                match e {
                    Some((c, p, t)) => {
                        if let Some(d) = f.deadline() && d < Micros(t) {
                            out.extend(f.expire(d).map(|c| (c, false)));
                        }
                        f.key(c, p, Micros(t), Micros(t), |c, p| out.push((c, p)));
                    }
                    None => out.extend(f.batch_end().map(|c| (c, false))),
                }
            }
            if let Some(d) = f.deadline() {
                out.extend(f.expire(d).map(|c| (c, false)));
            }
            out.extend(f.batch_end().map(|c| (c, false)));
            prop_assert_eq!(out, expected);
        }
    }
}
