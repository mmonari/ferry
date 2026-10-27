// from gliff crates/hypr-input/src/keymap.rs @ 089662f
//! xkb keymap construction and the per-session key state.

use std::time::Duration;

use xkbcommon::xkb;

use crate::{Error, Result, Target};

#[derive(Debug, Clone, Default)]
pub struct KeymapNames {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: Option<String>,
}

/// Compile a keymap from RMLVO names and return its text (format v1).
pub fn keymap_from_names(names: &KeymapNames) -> Result<String> {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(
        &ctx,
        &names.rules,
        &names.model,
        &names.layout,
        &names.variant,
        names.options.clone(),
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or_else(|| Error::Xkb(format!("cannot compile keymap {names:?}")))?;
    Ok(keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1))
}

impl KeymapNames {
    /// The names Hyprland compiles the main keyboard's keymap from, or the first keyboard's
    /// when none is main. `None` when the seat has no keyboard.
    pub fn of_main_keyboard(keyboards: &[ferry_hypr_ipc::Keyboard]) -> Option<Self> {
        let kb = keyboards
            .iter()
            .find(|k| k.main)
            .or_else(|| keyboards.first())?;
        Some(Self {
            rules: kb.rules.clone(),
            model: kb.model.clone(),
            layout: kb.layout.clone(),
            variant: kb.variant.clone(),
            options: Some(kb.options.clone()),
        })
    }
}

/// How long Hyprland gets to list its keyboards before a new keyboard falls back to plain US.
const DEVICES_TIMEOUT: Duration = Duration::from_millis(500);

/// This machine's keymap: the names of Hyprland's main keyboard, compiled here. With the same
/// libxkbcommon the text is byte for byte the one Hyprland serves for that keyboard (the
/// desktop's `us(intl)`, 2026-10-17), so in canonical form the two are one keymap.
pub fn system_keymap(target: &Target) -> Result<String> {
    let json = target
        .instance()?
        .request_within("j/devices", DEVICES_TIMEOUT)?;
    let keyboards = ferry_hypr_ipc::keyboards_from_json(&json)?;
    let names = KeymapNames::of_main_keyboard(&keyboards)
        .ok_or_else(|| Error::Xkb("Hyprland lists no keyboard".into()))?;
    tracing::debug!(
        layout = %names.layout,
        variant = %names.variant,
        options = names.options.as_deref().unwrap_or(""),
        "this machine's keymap"
    );
    keymap_from_names(&names)
}

/// The canonical form of a keymap text: compiled, then serialised again by
/// this machine's libxkbcommon. Two texts of one keymap compare equal once
/// canonical, and the canonical form is a fixed point.
///
/// fcitx5's virtual keyboard carries the round trip of the keyboard's
/// keymap, and the round trip is not the identity for every layout:
/// `us(intl)` with `shift:both_capslock_cancel` loses 8 bytes (a shared
/// modifier-map line) and keeps its behaviour. On a machine where Hyprland's
/// active keyboard flips between the two, the EIS keyboard sees both texts.
pub fn canonical_keymap(text: &str) -> Result<String> {
    Ok(KeyState::from_text(text)?
        .keymap
        .get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1))
}

/// Name of the keysym `evdev_code` gives on its first level in the first
/// layout, e.g. `Control_L`.
pub fn key_name(text: &str, evdev_code: u32) -> Result<String> {
    let keymap = KeyState::from_text(text)?.keymap;
    let syms = keymap.key_get_syms_by_level(xkb::Keycode::new(evdev_code + 8), 0, 0);
    Ok(syms
        .first()
        .map_or_else(String::new, |s| xkb::keysym_get_name(*s)))
}

/// Tracks modifier state for the virtual keyboard from raw key events.
pub struct KeyState {
    pub keymap: xkb::Keymap,
    state: xkb::State,
    pub text: String,
    last_mods: (u32, u32, u32, u32),
}

impl KeyState {
    pub fn from_text(text: &str) -> Result<Self> {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_string(
            &ctx,
            text.to_owned(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .ok_or_else(|| Error::Xkb("cannot compile keymap text".into()))?;
        let state = xkb::State::new(&keymap);
        Ok(Self {
            keymap,
            state,
            text: text.to_owned(),
            last_mods: (0, 0, 0, 0),
        })
    }

    pub fn default_us() -> Result<Self> {
        let text = keymap_from_names(&KeymapNames {
            layout: "us".into(),
            ..Default::default()
        })?;
        Self::from_text(&text)
    }

    /// The state for a keyboard created without a keymap: this machine's ([`system_keymap`]),
    /// or plain US when Hyprland cannot say which. The start-up reset's keyboard is one, and
    /// the keymap it carries is what Hyprland's seat, and fcitx5, show until the next real
    /// key: plain US there showed as a layout change on both machines.
    pub fn system(target: &Target) -> Result<Self> {
        match system_keymap(target) {
            Ok(text) => Self::from_text(&text),
            Err(e) => {
                tracing::warn!(error = %e, "cannot read this machine's keymap; using plain US");
                Self::default_us()
            }
        }
    }

    /// Apply a key event. Returns the new `(depressed, latched, locked, group)`
    /// when it changed.
    pub fn update(&mut self, evdev_code: u32, pressed: bool) -> Option<(u32, u32, u32, u32)> {
        let dir = if pressed {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        self.state
            .update_key(xkb::Keycode::new(evdev_code.saturating_add(8)), dir);
        let mods = self.mods();
        if mods != self.last_mods {
            self.last_mods = mods;
            Some(mods)
        } else {
            None
        }
    }

    /// Take over the state of `old`, a different keymap: its locked layout,
    /// the keys still `held`, then its locked modifiers (matched by name).
    /// Returns the resulting `(depressed, latched, locked, group)`.
    pub fn carry_over(
        &mut self,
        old: &KeyState,
        held: impl IntoIterator<Item = u32>,
    ) -> (u32, u32, u32, u32) {
        let layout = old.state.serialize_layout(xkb::STATE_LAYOUT_LOCKED);
        let layout = if layout < self.keymap.num_layouts() {
            layout
        } else {
            0
        };
        // Held keys are replayed in the carried layout, and before the
        // locked modifiers are set, so a held lock key does not count them
        // as its own and clear them on release.
        self.state.update_mask(0, 0, 0, 0, 0, layout);
        for code in held {
            self.state.update_key(
                xkb::Keycode::new(code.saturating_add(8)),
                xkb::KeyDirection::Down,
            );
        }
        let old_locked = old.state.serialize_mods(xkb::STATE_MODS_LOCKED);
        let mut locked = 0;
        for i in 0..old.keymap.num_mods() {
            if old_locked & (1 << i) == 0 {
                continue;
            }
            let index = self.keymap.mod_get_index(old.keymap.mod_get_name(i));
            if index != xkb::MOD_INVALID {
                locked |= 1 << index;
            }
        }
        self.state.update_mask(
            self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            self.state.serialize_mods(xkb::STATE_MODS_LATCHED),
            locked,
            self.state.serialize_layout(xkb::STATE_LAYOUT_DEPRESSED),
            self.state.serialize_layout(xkb::STATE_LAYOUT_LATCHED),
            layout,
        );
        self.last_mods = self.mods();
        self.last_mods
    }

    fn mods(&self) -> (u32, u32, u32, u32) {
        (
            self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            self.state.serialize_mods(xkb::STATE_MODS_LATCHED),
            self.state.serialize_mods(xkb::STATE_MODS_LOCKED),
            self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
        )
    }

    pub fn reset(&mut self) {
        self.state = xkb::State::new(&self.keymap);
        self.last_mods = (0, 0, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_us_keymap_text() {
        let text = keymap_from_names(&KeymapNames {
            layout: "us".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(text.contains("xkb_keymap"));
        assert!(text.contains("xkb_symbols"));
    }

    #[test]
    fn names_come_from_the_main_keyboard() {
        use ferry_hypr_ipc::Keyboard;
        let kb = |name: &str, layout: &str, main: bool| Keyboard {
            name: name.into(),
            layout: layout.into(),
            options: "compose:caps".into(),
            main,
            ..Default::default()
        };
        let names = KeymapNames::of_main_keyboard(&[
            kb("power-button", "us", false),
            kb("hl-virtual-keyboard-fcitx5", "br", true),
        ])
        .unwrap();
        assert_eq!(names.layout, "br");
        assert_eq!(names.options.as_deref(), Some("compose:caps"));
        // No main keyboard: the first one.
        let names = KeymapNames::of_main_keyboard(&[kb("a", "de", false), kb("b", "fr", false)]);
        assert_eq!(names.unwrap().layout, "de");
        assert!(KeymapNames::of_main_keyboard(&[]).is_none());
    }

    /// No Hyprland to ask (here, an instance that does not exist): plain US, as before.
    #[test]
    fn system_keymap_falls_back_to_plain_us() {
        let target = Target {
            display: None,
            instance: Some("ferry-test-no-such-instance".into()),
        };
        assert!(system_keymap(&target).is_err());
        let ks = KeyState::system(&target).unwrap();
        assert_eq!(ks.text, KeyState::default_us().unwrap().text);
    }

    /// The desktop's layout (first light): the keyboard's text and fcitx5's
    /// round trip of it differ, and both have one canonical form.
    #[test]
    fn two_texts_of_one_keymap_have_one_canonical_form() {
        let keyboard = keymap_from_names(&KeymapNames {
            layout: "us".into(),
            variant: "intl".into(),
            options: Some("compose:caps,shift:both_capslock_cancel".into()),
            ..Default::default()
        })
        .unwrap();
        let fcitx5 = canonical_keymap(&keyboard).unwrap();
        assert_ne!(
            keyboard, fcitx5,
            "libxkbcommon's round trip of this layout is the identity now: the alternation \
             this guards against needs another example"
        );
        assert_eq!(canonical_keymap(&fcitx5).unwrap(), fcitx5, "a fixed point");
        assert_eq!(
            canonical_keymap(&keyboard).unwrap(),
            canonical_keymap(&fcitx5).unwrap()
        );
    }

    #[test]
    fn different_layouts_stay_different() {
        let names = |layout: &str| KeymapNames {
            layout: layout.into(),
            ..Default::default()
        };
        let us = canonical_keymap(&keymap_from_names(&names("us")).unwrap()).unwrap();
        let br = canonical_keymap(&keymap_from_names(&names("br")).unwrap()).unwrap();
        assert_ne!(us, br);
        assert!(canonical_keymap("xkb_keymap { fake };").is_err());
    }

    #[test]
    fn shift_changes_modifiers() {
        let mut ks = KeyState::default_us().unwrap();
        const KEY_LEFTSHIFT: u32 = 42;
        let mods = ks
            .update(KEY_LEFTSHIFT, true)
            .expect("shift press changes mods");
        assert_ne!(mods.0, 0);
        assert!(ks.update(35, true).is_none());
        assert!(ks.update(35, false).is_none());
        let mods = ks
            .update(KEY_LEFTSHIFT, false)
            .expect("shift release changes mods");
        assert_eq!(mods.0, 0);
    }

    #[test]
    fn uploaded_keymap_options_apply() {
        let text = keymap_from_names(&KeymapNames {
            layout: "us".into(),
            options: Some("ctrl:nocaps".into()),
            ..Default::default()
        })
        .unwrap();
        let mut ks = KeyState::from_text(&text).unwrap();
        let control = 1 << ks.keymap.mod_get_index(xkb::MOD_NAME_CTRL);
        const KEY_CAPSLOCK: u32 = 58;
        let mods = ks
            .update(KEY_CAPSLOCK, true)
            .expect("caps press changes mods");
        assert_eq!(mods.0, control);
        assert_eq!(mods.2, 0);
    }

    #[test]
    fn carry_over_keeps_locks_and_held_keys() {
        const KEY_CAPSLOCK: u32 = 58;
        const KEY_LEFTCTRL: u32 = 29;
        let mut old = KeyState::default_us().unwrap();
        old.update(KEY_CAPSLOCK, true);
        old.update(KEY_CAPSLOCK, false);
        old.update(KEY_LEFTCTRL, true);
        let text = keymap_from_names(&KeymapNames {
            layout: "us,de".into(),
            ..Default::default()
        })
        .unwrap();
        let mut new = KeyState::from_text(&text).unwrap();
        let (depressed, _, locked, _) = new.carry_over(&old, [KEY_LEFTCTRL]);
        let lock = 1 << new.keymap.mod_get_index(xkb::MOD_NAME_CAPS);
        let control = 1 << new.keymap.mod_get_index(xkb::MOD_NAME_CTRL);
        assert_eq!(locked, lock);
        assert_eq!(depressed, control);
        let mods = new.update(KEY_LEFTCTRL, false).expect("ctrl release");
        assert_eq!((mods.0, mods.2), (0, lock));
    }

    #[test]
    fn carry_over_replays_held_keys_in_the_locked_layout() {
        const KEY_RIGHTALT: u32 = 100;
        let text = keymap_from_names(&KeymapNames {
            layout: "us,de".into(),
            ..Default::default()
        })
        .unwrap();
        let mut old = KeyState::from_text(&text).unwrap();
        old.state.update_mask(0, 0, 0, 0, 0, 1);
        old.update(KEY_RIGHTALT, true);
        let mut new = KeyState::from_text(&text).unwrap();
        let (depressed, _, _, group) = new.carry_over(&old, [KEY_RIGHTALT]);
        let level3 = 1 << new.keymap.mod_get_index("Mod5");
        assert_eq!(group, 1);
        assert_eq!(depressed, level3);
    }

    #[test]
    fn carry_over_keeps_the_lock_of_a_held_lock_key() {
        const KEY_CAPSLOCK: u32 = 58;
        let mut old = KeyState::default_us().unwrap();
        old.update(KEY_CAPSLOCK, true);
        let mut new = KeyState::default_us().unwrap();
        let lock = 1 << new.keymap.mod_get_index(xkb::MOD_NAME_CAPS);
        assert_eq!(new.carry_over(&old, [KEY_CAPSLOCK]).2, lock);
        new.update(KEY_CAPSLOCK, false);
        assert_eq!(new.mods().2, lock);
    }
}
