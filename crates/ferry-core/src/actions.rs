//! The caller-owned output buffer of the machines.

/// Effects of one `handle` call, in order. The caller drains it after every
/// call and keeps it for the next, so after warm-up no call allocates. One
/// event produces at most [`Actions::TYPICAL_MAX`] actions; more still works
/// (the buffer grows once) but means a design bug, and a debug assertion fires.
#[derive(Debug)]
pub struct Actions<A> {
    buf: Vec<A>,
}

impl<A> Actions<A> {
    /// Upper bound per event that the machines are designed to respect.
    pub const TYPICAL_MAX: usize = 32;

    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(Self::TYPICAL_MAX),
        }
    }

    pub fn push(&mut self, a: A) {
        debug_assert!(
            self.buf.len() < Self::TYPICAL_MAX,
            "a machine emitted more than TYPICAL_MAX ({}) actions for one event: a design bug (A-13)",
            Self::TYPICAL_MAX
        );
        self.buf.push(a);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn as_slice(&self) -> &[A] {
        &self.buf
    }

    /// Take the actions out in order, keeping the allocation.
    pub fn drain(&mut self) -> std::vec::Drain<'_, A> {
        self.buf.drain(..)
    }
}

impl<A> Default for Actions<A> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_max_actions_fit_without_growing() {
        let mut a = Actions::new();
        for i in 0..Actions::<usize>::TYPICAL_MAX {
            a.push(i);
        }
        assert_eq!(a.len(), Actions::<usize>::TYPICAL_MAX);
        assert_eq!(a.drain().count(), Actions::<usize>::TYPICAL_MAX);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "more than TYPICAL_MAX")]
    fn one_action_past_typical_max_is_a_design_bug() {
        let mut a = Actions::new();
        for i in 0..=Actions::<usize>::TYPICAL_MAX {
            a.push(i);
        }
    }
}
