//! The escape chord (NFR-A11Y-06), matched on evdev codes.
//!
//! Matching is positional (evdev codes, not keysyms), so the chord works
//! whatever keymap the source uses and needs no xkb in `ferry-core`.

use std::fmt;

use crate::geom::Side;

/// A modifier class; either the left or the right key satisfies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    Super,
    Ctrl,
    Alt,
    Shift,
}

impl Modifier {
    /// Evdev codes (left, right).
    pub fn codes(self) -> (u16, u16) {
        match self {
            Modifier::Ctrl => (29, 97),
            Modifier::Shift => (42, 54),
            Modifier::Alt => (56, 100),
            Modifier::Super => (125, 126),
        }
    }
}

/// `Super+Ctrl+Escape` by default: 2–3 modifiers plus one trigger key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscapeChord {
    pub modifiers: Vec<Modifier>,
    /// Evdev code of the non-modifier key (`Escape` = 1).
    pub trigger: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChordError {
    #[error("the escape chord needs at least 2 modifiers")]
    TooFewModifiers,
    #[error("the escape chord has at most 4 keys")]
    TooManyKeys,
    #[error("unknown key name {0:?}")]
    UnknownKey(String),
    #[error("the escape chord needs exactly one non-modifier key")]
    NoTrigger,
    #[error("the go chord is modifiers only (the arrow keys are its keys)")]
    GoHasKey,
}

impl EscapeChord {
    /// Parse `Super+Ctrl+Escape`-style text. Key names come from a static
    /// table (US positions: `Escape`, `F1`…`F12`, `A`…`Z`, `0`…`9`, `Space`,
    /// `Backspace`, `Delete`, `Home`, `End`, `Pause`).
    pub fn parse(s: &str) -> Result<Self, ChordError> {
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        if parts.len() > 4 {
            return Err(ChordError::TooManyKeys);
        }
        let mut modifiers = Vec::new();
        let mut trigger = None;
        for part in parts {
            if let Some(m) = Modifier::from_name(part) {
                if !modifiers.contains(&m) {
                    modifiers.push(m);
                }
                continue;
            }
            let code = key_code(part).ok_or_else(|| ChordError::UnknownKey(part.to_string()))?;
            if trigger.replace(code).is_some() {
                return Err(ChordError::NoTrigger);
            }
        }
        let trigger = trigger.ok_or(ChordError::NoTrigger)?;
        if modifiers.len() < 2 {
            return Err(ChordError::TooFewModifiers);
        }
        Ok(Self { modifiers, trigger })
    }

    /// True when `trigger` is pressed while every modifier class is held in
    /// `held_keys` (sorted evdev codes). Extra held keys do not prevent it.
    pub fn matches(&self, trigger: u16, held_keys: &[u16]) -> bool {
        trigger == self.trigger
            && self.modifiers.iter().all(|m| {
                let (l, r) = m.codes();
                held_keys.binary_search(&l).is_ok() || held_keys.binary_search(&r).is_ok()
            })
    }
}

/// The modifiers that, with an arrow key, move control to the machine on that side:
/// `Super+Ctrl+Alt` by default, 2–3 modifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoChord {
    pub modifiers: Vec<Modifier>,
}

/// The arrow keys (evdev) and the side each one goes to.
pub const ARROWS: [(u16, Side); 4] = [
    (105, Side::Left),
    (106, Side::Right),
    (103, Side::Top),
    (108, Side::Bottom),
];

impl GoChord {
    /// Parse `Super+Ctrl+Alt`-style text: modifiers only.
    pub fn parse(s: &str) -> Result<Self, ChordError> {
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        if parts.len() > 3 {
            return Err(ChordError::TooManyKeys);
        }
        let mut modifiers = Vec::new();
        for part in parts {
            match Modifier::from_name(part) {
                Some(m) if !modifiers.contains(&m) => modifiers.push(m),
                Some(_) => {}
                None if key_code(part).is_some() || arrow_name(part) => {
                    return Err(ChordError::GoHasKey);
                }
                None => return Err(ChordError::UnknownKey(part.to_string())),
            }
        }
        if modifiers.len() < 2 {
            return Err(ChordError::TooFewModifiers);
        }
        Ok(Self { modifiers })
    }

    /// The side of the arrow `trigger` when it is pressed while every modifier class is held in
    /// `held_keys` (sorted evdev codes).
    pub fn side(&self, trigger: u16, held_keys: &[u16]) -> Option<Side> {
        let side = ARROWS.iter().find(|(c, _)| *c == trigger)?.1;
        self.modifiers
            .iter()
            .all(|m| {
                let (l, r) = m.codes();
                held_keys.binary_search(&l).is_ok() || held_keys.binary_search(&r).is_ok()
            })
            .then_some(side)
    }
}

impl Default for GoChord {
    fn default() -> Self {
        Self {
            modifiers: vec![Modifier::Super, Modifier::Ctrl, Modifier::Alt],
        }
    }
}

impl fmt::Display for GoChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.modifiers.iter().map(|m| m.name()).collect();
        f.write_str(&names.join("+"))
    }
}

fn arrow_name(s: &str) -> bool {
    ["left", "right", "up", "down"].contains(&s.to_ascii_lowercase().as_str())
}

impl Modifier {
    /// The name Hyprland's `hl.bind` reads (`SUPER + CTRL + ALT + LEFT`).
    pub fn hypr_name(self) -> &'static str {
        match self {
            Modifier::Super => "SUPER",
            Modifier::Ctrl => "CTRL",
            Modifier::Alt => "ALT",
            Modifier::Shift => "SHIFT",
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "super" | "meta" | "logo" | "win" | "mod4" => Modifier::Super,
            "ctrl" | "control" => Modifier::Ctrl,
            "alt" | "mod1" => Modifier::Alt,
            "shift" => Modifier::Shift,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Modifier::Super => "Super",
            Modifier::Ctrl => "Ctrl",
            Modifier::Alt => "Alt",
            Modifier::Shift => "Shift",
        }
    }
}

/// Trigger names and their evdev codes (US positions).
const KEYS: &[(&str, u16)] = &[
    ("Escape", 1),
    ("1", 2),
    ("2", 3),
    ("3", 4),
    ("4", 5),
    ("5", 6),
    ("6", 7),
    ("7", 8),
    ("8", 9),
    ("9", 10),
    ("0", 11),
    ("Backspace", 14),
    ("Q", 16),
    ("W", 17),
    ("E", 18),
    ("R", 19),
    ("T", 20),
    ("Y", 21),
    ("U", 22),
    ("I", 23),
    ("O", 24),
    ("P", 25),
    ("A", 30),
    ("S", 31),
    ("D", 32),
    ("F", 33),
    ("G", 34),
    ("H", 35),
    ("J", 36),
    ("K", 37),
    ("L", 38),
    ("Z", 44),
    ("X", 45),
    ("C", 46),
    ("V", 47),
    ("B", 48),
    ("N", 49),
    ("M", 50),
    ("Space", 57),
    ("F1", 59),
    ("F2", 60),
    ("F3", 61),
    ("F4", 62),
    ("F5", 63),
    ("F6", 64),
    ("F7", 65),
    ("F8", 66),
    ("F9", 67),
    ("F10", 68),
    ("F11", 87),
    ("F12", 88),
    ("Home", 102),
    ("End", 107),
    ("Delete", 111),
    ("Pause", 119),
];

fn key_code(name: &str) -> Option<u16> {
    let alias = match name.to_ascii_lowercase().as_str() {
        "esc" => "escape",
        "del" => "delete",
        _ => "",
    };
    KEYS.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name) || n.eq_ignore_ascii_case(alias))
        .map(|&(_, c)| c)
}

impl Default for EscapeChord {
    fn default() -> Self {
        Self {
            modifiers: vec![Modifier::Super, Modifier::Ctrl],
            trigger: 1,
        }
    }
}

impl fmt::Display for EscapeChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for m in &self.modifiers {
            write!(f, "{}+", m.name())?;
        }
        match KEYS.iter().find(|&&(_, c)| c == self.trigger) {
            Some((name, _)) => f.write_str(name),
            None => write!(f, "{}", self.trigger),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_match() {
        let c = EscapeChord::parse("Super+Ctrl+Escape").expect("default");
        assert_eq!(c, EscapeChord::default());
        assert_eq!(c.to_string(), "Super+Ctrl+Escape");
        // Any L/R variant of each class.
        for (sup, ctrl) in [(125, 29), (126, 29), (125, 97), (126, 97)] {
            let mut held = vec![sup, ctrl];
            held.sort_unstable();
            assert!(c.matches(1, &held));
        }
        // Extra held keys do not prevent it.
        assert!(c.matches(1, &[29, 42, 125]));
        // A class missing, or another trigger.
        assert!(!c.matches(1, &[29, 42]));
        assert!(!c.matches(1, &[125]));
        assert!(!c.matches(2, &[29, 125]));
        // Case, spacing and aliases.
        let c = EscapeChord::parse(" ctrl + ALT + del ").expect("parse");
        assert_eq!(c.modifiers, vec![Modifier::Ctrl, Modifier::Alt]);
        assert_eq!(c.trigger, 111);
        assert_eq!(c.to_string(), "Ctrl+Alt+Delete");
        assert!(c.matches(111, &[29, 100]));
        let c = EscapeChord::parse("Super+Shift+Alt+F12").expect("parse");
        assert_eq!(c.trigger, 88);
        assert_eq!(EscapeChord::parse(&c.to_string()), Ok(c));
    }

    #[test]
    fn parse_rejects() {
        assert_eq!(
            EscapeChord::parse("Super+Escape"),
            Err(ChordError::TooFewModifiers)
        );
        assert_eq!(
            EscapeChord::parse("Super+Super+Escape"),
            Err(ChordError::TooFewModifiers)
        );
        assert_eq!(
            EscapeChord::parse("Super+Ctrl+Alt+Shift+Escape"),
            Err(ChordError::TooManyKeys)
        );
        assert_eq!(EscapeChord::parse("Super+Ctrl"), Err(ChordError::NoTrigger));
        assert_eq!(
            EscapeChord::parse("Super+Ctrl+A+B"),
            Err(ChordError::NoTrigger)
        );
        assert_eq!(
            EscapeChord::parse("Super+Ctrl+Hyper"),
            Err(ChordError::UnknownKey("Hyper".into()))
        );
    }
    #[test]
    fn go_chord_parses_modifiers_and_matches_arrows() {
        let c = GoChord::parse("Super+Ctrl+Alt").expect("default");
        assert_eq!(c, GoChord::default());
        assert_eq!(c.to_string(), "Super+Ctrl+Alt");
        let held = [29, 56, 125];
        assert_eq!(c.side(105, &held), Some(Side::Left));
        assert_eq!(c.side(106, &held), Some(Side::Right));
        assert_eq!(c.side(103, &held), Some(Side::Top));
        assert_eq!(c.side(108, &held), Some(Side::Bottom));
        // Right-hand modifiers count; a class missing or a non-arrow does not.
        assert_eq!(c.side(105, &[97, 100, 126]), Some(Side::Left));
        assert_eq!(c.side(105, &[29, 125]), None);
        assert_eq!(c.side(1, &held), None);
        assert_eq!(
            GoChord::parse(" ctrl + shift "),
            Ok(GoChord {
                modifiers: vec![Modifier::Ctrl, Modifier::Shift]
            })
        );
        assert_eq!(GoChord::parse("Super"), Err(ChordError::TooFewModifiers));
        assert_eq!(GoChord::parse("Super+Ctrl+Left"), Err(ChordError::GoHasKey));
        assert_eq!(
            GoChord::parse("Super+Ctrl+Escape"),
            Err(ChordError::GoHasKey)
        );
        assert_eq!(
            GoChord::parse("Super+Ctrl+Alt+Shift"),
            Err(ChordError::TooManyKeys)
        );
        assert_eq!(
            GoChord::parse("Super+Hyper"),
            Err(ChordError::UnknownKey("Hyper".into()))
        );
    }

    use proptest::prelude::*;

    fn arb_chord() -> impl Strategy<Value = EscapeChord> {
        let mods = proptest::sample::subsequence(
            vec![
                Modifier::Super,
                Modifier::Ctrl,
                Modifier::Alt,
                Modifier::Shift,
            ],
            2..=3,
        )
        .prop_shuffle();
        let trigger = proptest::sample::select(KEYS.iter().map(|&(_, c)| c).collect::<Vec<_>>());
        (mods, trigger).prop_map(|(modifiers, trigger)| EscapeChord { modifiers, trigger })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// `Display` is what `parse` reads: a valid chord survives the text round trip.
        #[test]
        fn a_chord_parses_back_from_its_text(c in arb_chord()) {
            prop_assert_eq!(EscapeChord::parse(&c.to_string()), Ok(c));
        }

        /// The chord text comes from `config.toml`: whatever it holds, parse answers `Ok` or
        /// `Err`, and an `Ok` always has the 2 to 3 modifiers the rule demands.
        #[test]
        fn chord_parse_never_panics(s in "\\PC{0,64}", soup in proptest::collection::vec(
            proptest::sample::select(vec!["Super", "ctrl", "ALT", "Shift", "Esc", "del", "F12", "A", "+", " ", "", "\u{0}", "é"]),
            0..12,
        )) {
            for text in [s, soup.concat()] {
                if let Ok(c) = EscapeChord::parse(&text) {
                    prop_assert!((2..=3).contains(&c.modifiers.len()));
                }
            }
        }
    }
}
