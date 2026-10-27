//! The keys and buttons this crate's devices currently hold down, as the
//! compositor was told. `release_all` works from this set, so it releases
//! exactly what was pressed, whatever the caller believes.

use std::collections::BTreeSet;

/// Evdev codes held on the virtual keyboard and on the virtual pointers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct HeldSet {
    keys: BTreeSet<u32>,
    buttons: BTreeSet<u32>,
}

impl HeldSet {
    /// Record a key edge. Returns true when the set changed (a repeated press
    /// or a release of a key not held changes nothing).
    pub(crate) fn key(&mut self, code: u32, pressed: bool) -> bool {
        if pressed {
            self.keys.insert(code)
        } else {
            self.keys.remove(&code)
        }
    }

    /// Record a button edge. Returns true when the set changed.
    pub(crate) fn button(&mut self, button: u32, pressed: bool) -> bool {
        if pressed {
            self.buttons.insert(button)
        } else {
            self.buttons.remove(&button)
        }
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = u32> + '_ {
        self.keys.iter().copied()
    }

    pub(crate) fn buttons(&self) -> impl Iterator<Item = u32> + '_ {
        self.buttons.iter().copied()
    }

    pub(crate) fn key_count(&self) -> usize {
        self.keys.len()
    }

    pub(crate) fn first_key(&self) -> Option<u32> {
        self.keys.first().copied()
    }

    pub(crate) fn first_button(&self) -> Option<u32> {
        self.buttons.first().copied()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_and_release_track_the_set() {
        let mut h = HeldSet::default();
        assert!(h.is_empty());
        assert!(h.key(42, true));
        assert!(h.key(125, true));
        assert!(h.button(0x110, true));
        assert_eq!(h.keys().collect::<Vec<_>>(), [42, 125]);
        assert_eq!(h.buttons().collect::<Vec<_>>(), [0x110]);
        assert!(!h.is_empty());
        assert!(h.key(42, false));
        assert_eq!(h.keys().collect::<Vec<_>>(), [125]);
        assert!(h.key(125, false));
        assert!(h.button(0x110, false));
        assert!(h.is_empty());
    }

    #[test]
    fn repeats_and_stray_releases_change_nothing() {
        let mut h = HeldSet::default();
        assert!(h.key(30, true));
        assert!(!h.key(30, true), "a repeated press is not a second hold");
        assert_eq!(h.key_count(), 1);
        assert!(!h.key(31, false), "a release of a key never pressed");
        assert!(!h.button(0x111, false));
        assert!(h.key(30, false));
        assert!(!h.key(30, false), "one press, one release");
        assert!(h.is_empty());
    }

    #[test]
    fn draining_one_by_one_empties_the_set_in_order() {
        let mut h = HeldSet::default();
        for code in [54, 29, 125] {
            h.key(code, true);
        }
        h.button(0x112, true);
        h.button(0x110, true);
        let mut released = Vec::new();
        while let Some(code) = h.first_key() {
            released.push(code);
            h.key(code, false);
        }
        assert_eq!(released, [29, 54, 125]);
        assert_eq!(h.first_button(), Some(0x110));
        h.button(0x110, false);
        assert_eq!(h.first_button(), Some(0x112));
        h.button(0x112, false);
        assert!(h.is_empty());
        assert_eq!(h.first_button(), None);
    }
}
