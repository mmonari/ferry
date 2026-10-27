//! Keymap texts at ferryd's edges: made canonical before either machine compares them, and
//! named by a short hash in the log.
//!
//! The machines treat "same text" as "same keymap". That is only true once the text is
//! canonical: compiled and serialised again by this machine's libxkbcommon
//! ([`ferry_inject::canonical_keymap`]). Two texts of one keymap reach ferryd on an Omarchy
//! machine with fcitx5: Hyprland's active keyboard flips between the real keyboard and fcitx5's
//! virtual keyboard, which carries fcitx5's round trip of the same keymap, and the EIS keyboard
//! follows it. For the desktop's `us(intl)` with `shift:both_capslock_cancel` the two are
//! 37 123 and 37 115 bytes. Compared raw, every flip was a new keymap: a frame to every peer
//! and a new virtual keyboard on the target (design §5.1 as built).
//!
//! - **Source** (`capture` thread): each EIS keymap is canonical before `SourceEvent::Keymap`.
//! - **Target** (`net` thread): each received `Keymap` text is canonical before the
//!   TargetMachine sees it, whatever the source's build or libxkbcommon did.
//!
//! A text that does not compile passes through unchanged, so ferry-inject still rejects it as
//! before.

use std::collections::VecDeque;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};

/// How many raw texts the cache remembers. The alternation has two; a burst of EIS re-adds
/// repeats the last one.
const RECENT: usize = 4;

/// A canonical keymap and what it came from, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canonical {
    pub text: String,
    pub raw_len: usize,
    pub raw_hash: String,
    /// The raw text compiled. False: `text` is the raw text, unchanged.
    pub compiled: bool,
}

impl Canonical {
    pub fn hash(&self) -> String {
        short_hash(&self.text)
    }
}

/// Canonical forms of the last few raw texts, newest first. Compiling a desktop keymap costs
/// about 1.3 ms on the desktop and more on the netbook; the alternation then costs it once per
/// text.
#[derive(Debug, Default)]
pub struct KeymapCanon {
    recent: VecDeque<(String, String, bool)>,
}

impl KeymapCanon {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn canonical(&mut self, raw: String) -> Canonical {
        let raw_len = raw.len();
        let raw_hash = short_hash(&raw);
        let (text, compiled) = match self.recent.iter().position(|(r, _, _)| *r == raw) {
            Some(i) => {
                let hit = self.recent.remove(i).expect("position is in range");
                let out = (hit.1.clone(), hit.2);
                self.recent.push_front(hit);
                out
            }
            None => {
                let (text, compiled) = match ferry_inject::canonical_keymap(&raw) {
                    Ok(c) => (c, true),
                    Err(_) => (raw.clone(), false),
                };
                self.recent.push_front((raw, text.clone(), compiled));
                self.recent.truncate(RECENT);
                (text, compiled)
            }
        };
        Canonical {
            text,
            raw_len,
            raw_hash,
            compiled,
        }
    }
}

/// The first 6 bytes of the text's SHA-256, in hex: enough to tell keymaps apart in a log,
/// and the same on every machine and build.
pub fn short_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..6].iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use ferry_core::config::SourceConfig;
    use ferry_core::source::{SourceAction, SourceEvent, SourceMachine};
    use ferry_core::target::{InjectCmd, TargetAction, TargetEvent, TargetMachine};
    use ferry_core::wire::Control;
    use ferry_core::{Actions, Micros, PeerId};
    use ferry_inject::{KeymapNames, keymap_from_names};

    use super::*;

    const DESK: PeerId = PeerId([0xd0; 32]);
    const NETBOOK: PeerId = PeerId([0x0b; 32]);

    fn names(layout: &str, variant: &str, options: &str) -> KeymapNames {
        KeymapNames {
            layout: layout.into(),
            variant: variant.into(),
            options: Some(options.into()),
            ..Default::default()
        }
    }

    /// The desktop's two texts of one keymap: its keyboard's, and fcitx5's round trip of it.
    fn desktop_pair() -> (String, String) {
        let keyboard = keymap_from_names(&names(
            "us",
            "intl",
            "compose:caps,shift:both_capslock_cancel",
        ))
        .unwrap();
        let fcitx5 = ferry_inject::canonical_keymap(&keyboard).unwrap();
        assert_ne!(keyboard, fcitx5, "the fixture needs two texts");
        (keyboard, fcitx5)
    }

    /// The desktop as source and the netbook as target, wired as ferryd wires them: raw EIS
    /// text → canonical → SourceMachine → frames → canonical → TargetMachine.
    struct Pair {
        canon_src: KeymapCanon,
        canon_tgt: KeymapCanon,
        src: SourceMachine,
        sout: Actions<SourceAction>,
        tgt: TargetMachine,
        tout: Actions<TargetAction>,
        now: Micros,
        /// `SetKeymap` texts the target sent to ferry-inject.
        swaps: Vec<String>,
    }

    impl Pair {
        fn new() -> Self {
            let mut p = Pair {
                canon_src: KeymapCanon::new(),
                canon_tgt: KeymapCanon::new(),
                src: SourceMachine::new(SourceConfig::default()),
                sout: Actions::new(),
                tgt: TargetMachine::new(),
                tout: Actions::new(),
                now: Micros::from_millis(10_000),
                swaps: Vec::new(),
            };
            p.connect();
            p
        }

        fn connect(&mut self) {
            self.tgt
                .handle(TargetEvent::PeerUp(DESK), self.now, &mut self.tout);
            self.source(SourceEvent::PeerUp(NETBOOK));
        }

        fn disconnect(&mut self) {
            self.tgt
                .handle(TargetEvent::PeerDown(DESK), self.now, &mut self.tout);
            self.target_actions();
            self.source(SourceEvent::PeerDown(NETBOOK));
        }

        /// Feed the source; deliver its frames to the target. Returns the Keymap frames.
        fn source(&mut self, ev: SourceEvent) -> usize {
            self.src.handle(ev, self.now, &mut self.sout);
            let frames: Vec<Control> = self
                .sout
                .drain()
                .filter_map(|a| match a {
                    SourceAction::Send { peer, msg } if peer == NETBOOK => Some(msg),
                    _ => None,
                })
                .collect();
            let mut keymaps = 0;
            for msg in frames {
                let msg = match msg {
                    Control::Keymap {
                        serial,
                        format,
                        text,
                    } => {
                        keymaps += 1;
                        Control::Keymap {
                            serial,
                            format,
                            text: self.canon_tgt.canonical(text).text,
                        }
                    }
                    m => m,
                };
                self.target(msg);
            }
            keymaps
        }

        fn eis_keymap(&mut self, raw: &str) -> usize {
            let text = self.canon_src.canonical(raw.to_owned()).text;
            self.source(SourceEvent::Keymap { text })
        }

        fn target(&mut self, msg: Control) {
            self.tgt.handle(
                TargetEvent::Control { peer: DESK, msg },
                self.now,
                &mut self.tout,
            );
            self.target_actions();
        }

        fn target_actions(&mut self) {
            for a in self.tout.drain() {
                if let TargetAction::Inject(InjectCmd::SetKeymap(t)) = a {
                    self.swaps.push(t);
                }
            }
        }

        /// The target entered as the source's crossing would: `Enter` naming its keymap.
        fn enter(&mut self, session: u32) {
            let serial = self.src.keymap_serial().expect("a keymap");
            self.enter_with(session, serial);
        }

        fn enter_with(&mut self, session: u32, serial: u32) {
            self.target(Control::Enter {
                session,
                x: 10.0,
                y: 0.0,
                keymap_serial: serial,
                motion_seq: 0,
            });
            assert_eq!(self.tgt.status().entered_by, Some(DESK));
        }
    }

    #[test]
    fn an_alternation_of_one_keymap_is_one_frame_per_connection_and_one_keyboard() {
        let (keyboard, fcitx5) = desktop_pair();
        let mut p = Pair::new();
        assert_eq!(p.eis_keymap(&keyboard), 1, "the first keymap goes out");
        // fcitx5 and the keyboard take turns as Hyprland's active keyboard, each with its
        // burst of EIS re-adds.
        for raw in [&fcitx5, &keyboard, &fcitx5, &keyboard, &keyboard, &fcitx5] {
            assert_eq!(p.eis_keymap(raw), 0);
        }
        p.enter(1);
        for raw in [&keyboard, &fcitx5, &keyboard, &fcitx5] {
            assert_eq!(p.eis_keymap(raw), 0);
        }
        assert_eq!(p.src.counters().keymaps_sent, 1);
        assert_eq!(p.tgt.counters().keymap_swaps, 1);
        assert_eq!(
            p.swaps,
            std::slice::from_ref(&fcitx5),
            "one keyboard, canonical"
        );
        assert_eq!(p.src.keymap_serial(), Some(1));

        // A new connection gets the keymap once; the target's keyboard already has it.
        p.disconnect();
        p.connect();
        for raw in [&fcitx5, &keyboard, &fcitx5] {
            assert_eq!(p.eis_keymap(raw), 0);
        }
        assert_eq!(p.src.counters().keymaps_sent, 2);
        p.enter(2);
        assert_eq!(p.tgt.counters().keymap_swaps, 1);
    }

    #[test]
    fn a_real_layout_change_mid_session_reaches_the_target_at_once() {
        let us = keymap_from_names(&names("us", "", "")).unwrap();
        let br = keymap_from_names(&names("br", "", "")).unwrap();
        let mut p = Pair::new();
        p.eis_keymap(&us);
        p.enter(1);
        assert_eq!(p.swaps.len(), 1);
        // us → br while entered: one frame, and the target's next keyboard has br before any
        // later frame is applied.
        assert_eq!(p.eis_keymap(&br), 1);
        assert_eq!(p.src.keymap_serial(), Some(2));
        assert_eq!(p.tgt.counters().keymap_swaps, 2);
        assert_eq!(
            p.swaps.last(),
            Some(&ferry_inject::canonical_keymap(&br).unwrap())
        );
        // And back: a change again.
        assert_eq!(p.eis_keymap(&us), 1);
        assert_eq!(p.tgt.counters().keymap_swaps, 3);
    }

    #[test]
    fn the_target_keeps_its_keyboard_for_a_raw_text_of_the_same_keymap() {
        // A source that sends raw texts with a new serial each (an older build, fake-peer):
        // the target's own canonical form decides.
        let (keyboard, fcitx5) = desktop_pair();
        let mut p = Pair::new();
        let text = p.canon_tgt.canonical(keyboard.clone()).text;
        p.target(Control::Keymap {
            serial: 1,
            format: 1,
            text,
        });
        p.enter_with(1, 1);
        for (serial, raw) in [(2, &fcitx5), (3, &keyboard), (4, &fcitx5)] {
            let text = p.canon_tgt.canonical(raw.clone()).text;
            p.target(Control::Keymap {
                serial,
                format: 1,
                text,
            });
        }
        assert_eq!(p.tgt.counters().keymap_swaps, 1);
    }

    #[test]
    fn a_text_that_does_not_compile_passes_unchanged() {
        let mut c = KeymapCanon::new();
        let out = c.canonical("xkb_keymap { fake };".into());
        assert_eq!(out.text, "xkb_keymap { fake };");
        assert!(!out.compiled);
        assert_eq!(out.raw_hash, out.hash());
    }

    #[test]
    fn the_cache_returns_the_same_canonical_text() {
        let (keyboard, fcitx5) = desktop_pair();
        let mut c = KeymapCanon::new();
        let a = c.canonical(keyboard.clone());
        let b = c.canonical(fcitx5.clone());
        let a2 = c.canonical(keyboard);
        assert_eq!(a.text, b.text);
        assert_eq!(a, a2);
        assert_ne!(a.raw_hash, b.raw_hash);
        assert_eq!(a.hash(), b.raw_hash, "fcitx5's text is the canonical one");
        assert_eq!(short_hash("").len(), 12);
    }
}
