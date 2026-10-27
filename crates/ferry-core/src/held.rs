//! The held set: keys and buttons currently down. Lifted from
//! `spikes/s10-stuck-keys/src/wire.rs` (`Held`), with fixed caps.

/// At most this many keys are tracked as held. A press beyond it is
/// reported as [`Edge::AlreadyDown`] and so dropped, never forwarded: an
/// untracked press could never be released by a snapshot. No keyboard
/// reaches it in practice.
pub const MAX_HELD_KEYS: usize = 32;
/// At most this many buttons are tracked as held.
pub const MAX_HELD_BUTTONS: usize = 16;

/// Sorted, de-duplicated evdev codes. On the source: what EIS says is down
/// since the crossing. On the target: what this peer pressed on its virtual
/// devices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldSet {
    keys: Vec<u16>,
    buttons: Vec<u16>,
}

/// What a key or button edge did to the set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// The set changed: forward or inject it.
    Changed,
    /// A press of something already down: drop it.
    AlreadyDown,
    /// A release of something not down (an orphan): drop it.
    NotDown,
}

impl HeldSet {
    pub fn key(&mut self, code: u16, pressed: bool) -> Edge {
        edge(&mut self.keys, MAX_HELD_KEYS, code, pressed)
    }

    pub fn button(&mut self, code: u16, pressed: bool) -> Edge {
        edge(&mut self.buttons, MAX_HELD_BUTTONS, code, pressed)
    }

    pub fn keys(&self) -> &[u16] {
        &self.keys
    }

    pub fn buttons(&self) -> &[u16] {
        &self.buttons
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }

    pub fn clear(&mut self) {
        self.keys.clear();
        self.buttons.clear();
    }

    /// Target side: release everything held here that `keys`/`buttons` lack,
    /// calling `release(is_button, code)` for each. Never presses.
    pub fn reconcile(&mut self, keys: &[u16], buttons: &[u16], mut release: impl FnMut(bool, u16)) {
        self.keys.retain(|k| {
            let keep = keys.contains(k);
            if !keep {
                release(false, *k);
            }
            keep
        });
        self.buttons.retain(|b| {
            let keep = buttons.contains(b);
            if !keep {
                release(true, *b);
            }
            keep
        });
    }
}

/// One edge against one sorted list; a press past `cap` is dropped.
fn edge(set: &mut Vec<u16>, cap: usize, code: u16, pressed: bool) -> Edge {
    match (set.binary_search(&code), pressed) {
        (Ok(_), true) => Edge::AlreadyDown,
        (Err(_), false) => Edge::NotDown,
        (Ok(i), false) => {
            set.remove(i);
            Edge::Changed
        }
        (Err(_), true) if set.len() >= cap => Edge::AlreadyDown,
        (Err(i), true) => {
            set.insert(i, code);
            Edge::Changed
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn edges() {
        let mut h = HeldSet::default();
        assert_eq!(h.key(125, true), Edge::Changed);
        assert_eq!(h.key(42, true), Edge::Changed);
        assert_eq!(h.key(42, true), Edge::AlreadyDown);
        assert_eq!(h.keys(), &[42, 125]);
        assert_eq!(h.key(30, false), Edge::NotDown);
        assert_eq!(h.button(272, true), Edge::Changed);
        assert_eq!(h.button(273, false), Edge::NotDown);
        assert_eq!(h.key(42, false), Edge::Changed);
        assert_eq!(h.keys(), &[125]);
        assert!(!h.is_empty());
        h.clear();
        assert!(h.is_empty());
    }

    #[test]
    fn caps_drop_the_press() {
        let mut h = HeldSet::default();
        for c in 0..MAX_HELD_KEYS as u16 {
            assert_eq!(h.key(c, true), Edge::Changed);
        }
        assert_eq!(h.key(500, true), Edge::AlreadyDown);
        assert_eq!(h.key(500, false), Edge::NotDown);
        for c in 0..MAX_HELD_BUTTONS as u16 {
            assert_eq!(h.button(0x110 + c, true), Edge::Changed);
        }
        assert_eq!(h.button(0x200, true), Edge::AlreadyDown);
    }

    #[test]
    fn snapshot_releases_missing_and_never_presses() {
        // The S10 values.
        let mut h = HeldSet::default();
        h.key(125, true);
        h.key(42, true);
        h.button(272, true);
        let mut rel = Vec::new();
        h.reconcile(&[125, 30], &[], |b, c| rel.push((b, c)));
        assert_eq!(rel, vec![(false, 42), (true, 272)]);
        assert_eq!(h.keys(), &[125]);
        assert!(h.buttons().is_empty());
        rel.clear();
        h.reconcile(&[125], &[], |b, c| rel.push((b, c)));
        assert!(rel.is_empty());
    }

    fn arb_edges() -> impl Strategy<Value = Vec<(bool, u16, bool)>> {
        proptest::collection::vec((any::<bool>(), 0u16..48, any::<bool>()), 0..200)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2048))]

        #[test]
        fn release_all_empties(edges in arb_edges()) {
            let mut h = HeldSet::default();
            for (button, code, pressed) in edges {
                if button { h.button(code, pressed); } else { h.key(code, pressed); }
                prop_assert!(h.keys().windows(2).all(|w| w[0] < w[1]));
                prop_assert!(h.buttons().windows(2).all(|w| w[0] < w[1]));
                prop_assert!(h.keys().len() <= MAX_HELD_KEYS);
                prop_assert!(h.buttons().len() <= MAX_HELD_BUTTONS);
            }
            h.clear();
            prop_assert!(h.is_empty());
        }

        #[test]
        fn reconcile_never_presses(
            edges in arb_edges(),
            keys in proptest::collection::vec(0u16..48, 0..32),
            buttons in proptest::collection::vec(0u16..48, 0..16),
        ) {
            let mut h = HeldSet::default();
            for (button, code, pressed) in edges {
                if button { h.button(code, pressed); } else { h.key(code, pressed); }
            }
            let before = h.clone();
            let mut released = Vec::new();
            h.reconcile(&keys, &buttons, |b, c| released.push((b, c)));
            for k in h.keys() {
                prop_assert!(before.keys().contains(k) && keys.contains(k));
            }
            for b in h.buttons() {
                prop_assert!(before.buttons().contains(b) && buttons.contains(b));
            }
            for (b, c) in released {
                let was = if b { before.buttons() } else { before.keys() };
                prop_assert!(was.contains(&c));
            }
        }
    }
}
