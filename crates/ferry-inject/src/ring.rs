//! The inject thread's bounded queue of commands that have not reached the
//! compositor yet. It fills while the Wayland socket would block, and within
//! one wake-up before the single flush.
//!
//! Order is kept. Only the newest entry can absorb a new command, and only
//! for motion and scroll, so a key or button between two motions keeps both
//! sides of it. Keys and buttons are never merged or dropped: when the ring is
//! full the caller gets the command back and must release, not discard.

use std::collections::VecDeque;

use crate::InputCmd;

/// Entries the ring holds (architecture §3: "an ordered ring of 256").
pub(crate) const RING_CAP: usize = 256;

#[derive(Debug)]
pub(crate) enum Push {
    /// Appended.
    Queued,
    /// Merged into the newest entry; the ring did not grow.
    Coalesced,
    /// No room and no merge possible. The command is handed back.
    Full(InputCmd),
}

#[derive(Debug)]
pub(crate) struct Ring {
    queue: VecDeque<InputCmd>,
    cap: usize,
}

impl Default for Ring {
    fn default() -> Self {
        Self::with_capacity(RING_CAP)
    }
}

impl Ring {
    pub(crate) fn with_capacity(cap: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(cap),
            cap,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }

    pub(crate) fn push(&mut self, cmd: InputCmd) -> Push {
        let cmd = match self.queue.back_mut() {
            Some(tail) => match merge(tail, cmd) {
                Ok(()) => return Push::Coalesced,
                Err(cmd) => cmd,
            },
            None => cmd,
        };
        if self.queue.len() >= self.cap {
            return Push::Full(cmd);
        }
        self.queue.push_back(cmd);
        Push::Queued
    }

    pub(crate) fn pop_front(&mut self) -> Option<InputCmd> {
        self.queue.pop_front()
    }

    /// Put back a command that could not be executed yet. Never refused: the
    /// slot it frees on `pop_front` is the one it takes.
    pub(crate) fn push_front(&mut self, cmd: InputCmd) {
        self.queue.push_front(cmd);
    }

    /// Drop the queued *input*, then queue `head` ahead of what stays: the
    /// newest `SetLayout` and `SetKeymap` (state, not input: losing them
    /// would leave the wrong layout or keymap in force) and every `Sync`, so a
    /// caller waiting in `sync()` is answered, not left hanging.
    pub(crate) fn restart_with(&mut self, head: impl IntoIterator<Item = InputCmd>) {
        let mut layout = None;
        let mut keymap = None;
        let mut syncs = Vec::new();
        for cmd in self.queue.drain(..) {
            match cmd {
                InputCmd::SetLayout(_) => layout = Some(cmd),
                InputCmd::SetKeymap(_) => keymap = Some(cmd),
                InputCmd::Sync(_) => syncs.push(cmd),
                _ => {}
            }
        }
        self.queue.extend(head);
        self.queue.extend(layout);
        self.queue.extend(keymap);
        self.queue.extend(syncs);
    }

    pub(crate) fn clear(&mut self) {
        self.queue.clear();
    }
}

/// Fold `new` into `tail` when they are the same kind of motion or scroll.
fn merge(tail: &mut InputCmd, new: InputCmd) -> Result<(), InputCmd> {
    match (tail, new) {
        (InputCmd::Motion { x, y }, InputCmd::Motion { x: nx, y: ny }) => {
            *x = nx;
            *y = ny;
            Ok(())
        }
        (InputCmd::MotionRel { dx, dy }, InputCmd::MotionRel { dx: ndx, dy: ndy }) => {
            *dx += ndx;
            *dy += ndy;
            Ok(())
        }
        (
            InputCmd::Axis {
                axis,
                source,
                value,
                value120,
            },
            InputCmd::Axis {
                axis: n_axis,
                source: n_source,
                value: n_value,
                value120: n_value120,
            },
        ) if *axis == n_axis && *source == n_source => match (&mut *value120, n_value120) {
            (None, None) => {
                *value += n_value;
                Ok(())
            }
            (Some(v), Some(n)) => {
                *v = v.saturating_add(n);
                *value += n_value;
                Ok(())
            }
            (_, n_value120) => Err(InputCmd::Axis {
                axis: n_axis,
                source: n_source,
                value: n_value,
                value120: n_value120,
            }),
        },
        (InputCmd::SetLayout(layout), InputCmd::SetLayout(new_layout)) => {
            *layout = new_layout;
            Ok(())
        }
        (_, new) => Err(new),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Axis, AxisSource, Layout};

    fn key(code: u32, pressed: bool) -> InputCmd {
        InputCmd::Key { code, pressed }
    }

    fn drain(ring: &mut Ring) -> Vec<InputCmd> {
        std::iter::from_fn(|| ring.pop_front()).collect()
    }

    #[test]
    fn consecutive_motion_keeps_only_the_newest_position() {
        let mut r = Ring::default();
        assert!(matches!(
            r.push(InputCmd::Motion { x: 1.0, y: 1.0 }),
            Push::Queued
        ));
        for i in 2..100 {
            let p = r.push(InputCmd::Motion {
                x: f64::from(i),
                y: 5.0,
            });
            assert!(matches!(p, Push::Coalesced));
        }
        assert_eq!(r.len(), 1);
        assert!(matches!(
            drain(&mut r)[..],
            [InputCmd::Motion { x: 99.0, y: 5.0 }]
        ));
    }

    #[test]
    fn relative_motion_sums() {
        let mut r = Ring::default();
        r.push(InputCmd::MotionRel { dx: 1.5, dy: -1.0 });
        r.push(InputCmd::MotionRel { dx: 2.0, dy: 4.0 });
        match drain(&mut r)[..] {
            [InputCmd::MotionRel { dx, dy }] => assert_eq!((dx, dy), (3.5, 3.0)),
            ref other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_key_between_motions_keeps_both_sides_in_order() {
        let mut r = Ring::default();
        r.push(InputCmd::Motion { x: 1.0, y: 1.0 });
        r.push(key(30, true));
        r.push(InputCmd::Motion { x: 2.0, y: 2.0 });
        r.push(InputCmd::Motion { x: 3.0, y: 3.0 });
        let v = drain(&mut r);
        assert!(matches!(
            v[..],
            [
                InputCmd::Motion { x: 1.0, .. },
                InputCmd::Key {
                    code: 30,
                    pressed: true
                },
                InputCmd::Motion { x: 3.0, .. }
            ]
        ));
    }

    #[test]
    fn keys_and_buttons_are_never_merged() {
        let mut r = Ring::default();
        r.push(key(30, true));
        r.push(key(30, true));
        r.push(key(30, false));
        r.push(InputCmd::Button {
            button: 0x110,
            pressed: true,
        });
        r.push(InputCmd::Button {
            button: 0x110,
            pressed: true,
        });
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn scroll_sums_only_for_the_same_axis_and_source() {
        let axis = |axis, source, value, value120| InputCmd::Axis {
            axis,
            source,
            value,
            value120,
        };
        let mut r = Ring::default();
        r.push(axis(Axis::Vertical, AxisSource::Wheel, 0.0, Some(120)));
        assert!(matches!(
            r.push(axis(Axis::Vertical, AxisSource::Wheel, 0.0, Some(-30))),
            Push::Coalesced
        ));
        assert!(matches!(
            r.push(axis(Axis::Horizontal, AxisSource::Wheel, 0.0, Some(120))),
            Push::Queued
        ));
        assert!(matches!(
            r.push(axis(Axis::Horizontal, AxisSource::Finger, 2.0, None)),
            Push::Queued
        ));
        assert!(matches!(
            r.push(axis(Axis::Horizontal, AxisSource::Finger, 3.0, None)),
            Push::Coalesced
        ));
        let v = drain(&mut r);
        assert!(matches!(
            v[0],
            InputCmd::Axis {
                value120: Some(90),
                ..
            }
        ));
        assert_eq!(v.len(), 3);
        assert!(matches!(v[2], InputCmd::Axis { value, .. } if value == 5.0));
    }

    #[test]
    fn a_full_ring_hands_the_key_back() {
        let mut r = Ring::with_capacity(3);
        for code in 1..=3 {
            assert!(matches!(r.push(key(code, true)), Push::Queued));
        }
        match r.push(key(4, true)) {
            Push::Full(InputCmd::Key { code: 4, .. }) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(r.len(), 3);
        // Motion still merges into a motion tail when the ring is full.
        let mut r = Ring::with_capacity(1);
        r.push(InputCmd::MotionRel { dx: 1.0, dy: 0.0 });
        assert!(matches!(
            r.push(InputCmd::MotionRel { dx: 1.0, dy: 0.0 }),
            Push::Coalesced
        ));
    }

    #[test]
    fn restart_keeps_the_newest_keymap_and_layout() {
        let mut r = Ring::default();
        r.push(InputCmd::SetKeymap("old".into()));
        r.push(key(30, true));
        r.push(InputCmd::SetKeymap("new".into()));
        r.push(InputCmd::SetLayout(Layout::default()));
        r.push(InputCmd::Motion { x: 1.0, y: 1.0 });
        r.restart_with([InputCmd::ReleaseAll]);
        let v = drain(&mut r);
        assert!(matches!(
            &v[..],
            [InputCmd::ReleaseAll, InputCmd::SetLayout(_), InputCmd::SetKeymap(k)] if k == "new"
        ));
    }

    #[test]
    fn restart_drops_input_but_answers_every_sync() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut r = Ring::default();
        r.push(key(30, true));
        r.push(InputCmd::Sync(tx));
        r.push(key(31, true));
        r.restart_with([InputCmd::ReleaseAll]);
        let v = drain(&mut r);
        assert!(matches!(v[..], [InputCmd::ReleaseAll, InputCmd::Sync(_)]));
        drop(v);
        // The sender was kept, then dropped with the drain: nothing was lost
        // before the drop, so the receiver sees a closed channel, not a hang.
        assert!(rx.recv().is_err());
    }

    #[test]
    fn a_layout_replaces_the_queued_layout() {
        let mut r = Ring::default();
        r.push(InputCmd::SetLayout(Layout::default()));
        let l = Layout {
            outputs: vec![crate::OutputRect {
                name: "A".into(),
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            }],
        };
        assert!(matches!(
            r.push(InputCmd::SetLayout(l.clone())),
            Push::Coalesced
        ));
        match &drain(&mut r)[..] {
            [InputCmd::SetLayout(got)] => assert_eq!(got, &l),
            other => panic!("{other:?}"),
        }
    }
}
