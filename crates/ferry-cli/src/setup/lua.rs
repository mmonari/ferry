//! The Hyprland config `ferry setup` owns: `~/.config/hypr/ferry.lua`, made of blocks, and the
//! one `require("hypr.ferry")` line it appends to `hyprland.lua`.
//!
//! `ferry.lua` is ours: its first line says so, and a file without that line is somebody else's
//! and is never touched. Each block sits between `-- ferry:begin <id>` and `-- ferry:end <id>`,
//! so `--undo` removes exactly what setup added, one step at a time. `hyprland.lua` is edited
//! once, by appending, and the append is exactly reversible.

use ferry_core::chord::{ARROWS, EscapeChord, GoChord};
use ferry_core::geom::Side;

/// The first line of a `ferry.lua` that setup wrote.
pub const HEADER: &str =
    "-- Written by `ferry setup`; `ferry setup --undo` removes this file and its require line.";
/// What `hyprland.lua` loads.
pub const REQUIRE: &str = r#"require("hypr.ferry")"#;
const REQUIRE_NOTE: &str = "-- ferry: loads ferry.lua (`ferry setup --undo` removes this line)";
const REQUIRE_NOTE_NO_NL: &str = "-- ferry: loads ferry.lua (`ferry setup --undo` removes this line; the file had no final newline)";
/// Where xdph lives on Arch: the binary Hyprland checks `input-capture` permission for on the
/// portal path (the capturing client is xdph, not ferryd).
pub const XDPH_BINARY: &str = "/usr/lib/xdg-desktop-portal-hyprland";

/// One thing setup puts in `ferry.lua`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    EscapeBind,
    /// The go chord with each arrow key runs `ferry go <side>`. While this machine
    /// captures, Hyprland skips them (no `allow_input_capture`): ferryd matches the chord then.
    GoBinds,
    ReleasePressed,
    Permission,
}

impl Block {
    pub fn id(self) -> &'static str {
        match self {
            Block::EscapeBind => "escape-bind",
            Block::GoBinds => "go-binds",
            Block::ReleasePressed => "release-pressed-on-close",
            Block::Permission => "capture-permission",
        }
    }

    /// The Lua of the block, without markers.
    fn lua(self, chords: &Chords) -> String {
        match self {
            Block::EscapeBind => format!(
                "-- Escape bind: returns input to this machine even if ferryd hangs.\n\
                 hl.bind(\"{chord}\", hl.dsp.release_input_capture(), {{ allow_input_capture = true, description = \"Ferry: release input capture\" }})\n",
                chord = chords.escape
            ),
            Block::GoBinds => {
                let mut out = String::from(
                    "-- Go binds: move the keyboard and mouse to the machine on that side.\n",
                );
                for (_, side) in ARROWS {
                    let name = go_name(side);
                    out.push_str(&format!(
                        "hl.bind(\"{mods} + {key}\", hl.dsp.exec_cmd(\"ferry go {name}\"), {{ description = \"{GO_DESCRIPTION}{name}\" }})\n",
                        mods = hypr_mods(&chords.go),
                        key = name.to_ascii_uppercase(),
                    ));
                }
                out
            }
            Block::ReleasePressed => "-- Clients see key-ups when a virtual keyboard dies.\n\
                 hl.config({ input = { virtualkeyboard = { release_pressed_on_close = true } } })\n"
                .to_string(),
            Block::Permission => format!(
                "-- Input capture is enforced (ecosystem:enforce_permissions). This rule allows xdph, the\n\
                 -- portal backend that captures for ferryd. Hyprland reads it only when it starts, not on a\n\
                 -- reload: log out and back in once. Until then, answer Hyprland's dialog at the first crossing.\n\
                 hl.permission({{ binary = \"{XDPH_BINARY}\", type = \"input-capture\", mode = \"allow\" }})\n"
            ),
        }
    }

    /// The block as it stands in the file, markers included, every line ending in `\n`.
    pub fn text(self, chord: &Chords) -> String {
        format!(
            "-- ferry:begin {id}\n{lua}-- ferry:end {id}\n",
            id = self.id(),
            lua = self.lua(chord)
        )
    }
}

/// What the blocks are written with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chords {
    /// The escape chord in Hyprland's syntax ([`hypr_chord`]).
    pub escape: String,
    pub go: GoChord,
}

/// The description of each go bind, before the side: `Ferry: go left`. Setup finds its binds by it.
pub const GO_DESCRIPTION: &str = "Ferry: go ";

/// The `ferry go` word for a side, which is also the arrow key's name in Hyprland.
pub fn go_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
        Side::Top => "up",
        Side::Bottom => "down",
    }
}

/// `Super+Ctrl+Alt` as Hyprland's bind syntax: `SUPER + CTRL + ALT`.
pub fn hypr_mods(go: &GoChord) -> String {
    go.modifiers
        .iter()
        .map(|m| m.hypr_name())
        .collect::<Vec<_>>()
        .join(" + ")
}

/// `Super+Ctrl+Escape` as Hyprland's bind syntax: `SUPER + CTRL + ESCAPE`.
pub fn hypr_chord(chord: &EscapeChord) -> String {
    chord
        .to_string()
        .split('+')
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Whether `text` is a `ferry.lua` that setup wrote.
pub fn is_ours(text: &str) -> bool {
    text.lines().next() == Some(HEADER)
}

/// A new `ferry.lua` holding `blocks`.
pub fn compose(blocks: &[Block], chord: &Chords) -> String {
    let mut out = format!("{HEADER}\n");
    for b in blocks {
        out.push('\n');
        out.push_str(&b.text(chord));
    }
    out
}

/// Whether the block is in the file.
pub fn has_block(text: &str, b: Block) -> bool {
    let begin = format!("-- ferry:begin {}", b.id());
    text.lines().any(|l| l == begin)
}

/// The file with `b` appended.
pub fn add_block(text: &str, b: Block, chord: &Chords) -> String {
    let mut out = text.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&b.text(chord));
    out
}

/// The file without `b` (its lines and the blank line before them). Other lines stay as they are.
pub fn remove_block(text: &str, b: Block) -> String {
    let begin = format!("-- ferry:begin {}", b.id());
    let end = format!("-- ferry:end {}", b.id());
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut inside = false;
    for l in lines {
        let bare = l.trim_end_matches('\n');
        if !inside && bare == begin {
            inside = true;
            if out.last().is_some_and(|p| p.trim().is_empty()) {
                out.pop();
            }
            continue;
        }
        if inside {
            if bare == end {
                inside = false;
            }
            continue;
        }
        out.push(l);
    }
    out.concat()
}

/// No block is left: the file is only the header and blank lines.
pub fn is_empty(text: &str) -> bool {
    text.lines().skip(1).all(|l| l.trim().is_empty())
}

/// Whether `hyprland.lua` already loads `hypr.ferry` (a commented line does not count).
pub fn requires_ferry(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim();
        !t.starts_with("--") && (t.contains(REQUIRE) || t.contains("require('hypr.ferry')"))
    })
}

/// What `hyprland.lua` gets: a note and the require line, after a blank line.
fn require_suffix(no_final_newline: bool) -> String {
    let note = if no_final_newline {
        REQUIRE_NOTE_NO_NL
    } else {
        REQUIRE_NOTE
    };
    format!("\n{note}\n{REQUIRE}\n")
}

/// `hyprland.lua` with the require line appended, and the lines that were added (for the diff).
pub fn append_require(text: &str) -> (String, String) {
    let no_nl = !text.is_empty() && !text.ends_with('\n');
    let suffix = require_suffix(no_nl);
    let mut out = text.to_string();
    if no_nl {
        out.push('\n');
    }
    out.push_str(&suffix);
    (out, suffix)
}

/// Undoes [`append_require`] exactly; `None` when the tail is not ours.
pub fn strip_require(text: &str) -> Option<String> {
    for no_nl in [false, true] {
        if let Some(head) = text.strip_suffix(&require_suffix(no_nl)) {
            return Some(if no_nl {
                head.strip_suffix('\n').unwrap_or(head).to_string()
            } else {
                head.to_string()
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESCAPE: &str = "SUPER + CTRL + ESCAPE";

    fn chords() -> Chords {
        Chords {
            escape: ESCAPE.into(),
            go: GoChord::default(),
        }
    }

    #[test]
    fn the_go_binds_run_ferry_go_for_each_arrow() {
        let t = Block::GoBinds.text(&chords());
        for side in ["left", "right", "up", "down"] {
            let line = format!(
                r#"hl.bind("SUPER + CTRL + ALT + {}", hl.dsp.exec_cmd("ferry go {side}"), {{ description = "Ferry: go {side}" }})"#,
                side.to_ascii_uppercase()
            );
            assert!(t.contains(&line), "{line}\n{t}");
        }
        // No allow_input_capture: while this machine captures, ferryd matches the chord.
        assert!(!t.contains("allow_input_capture"));
    }

    #[test]
    fn the_chord_is_written_in_hyprlands_syntax() {
        assert_eq!(hypr_chord(&EscapeChord::default()), ESCAPE);
        let c = EscapeChord::parse("Ctrl+Alt+Del").unwrap();
        assert_eq!(hypr_chord(&c), "CTRL + ALT + DELETE");
    }

    #[test]
    fn blocks_add_and_remove_without_touching_other_lines() {
        let mut t = compose(&[Block::EscapeBind], &chords());
        assert!(is_ours(&t) && has_block(&t, Block::EscapeBind));
        t.push_str("\n-- the user's own line\nhl.config({})\n");
        let t = add_block(&t, Block::ReleasePressed, &chords());
        assert!(has_block(&t, Block::ReleasePressed));
        let t = remove_block(&t, Block::EscapeBind);
        assert!(!has_block(&t, Block::EscapeBind) && has_block(&t, Block::ReleasePressed));
        assert!(t.contains("-- the user's own line\nhl.config({})"));
        let t = remove_block(&t, Block::ReleasePressed);
        assert!(t.contains("hl.config({})"), "{t}");
        assert!(!is_empty(&t), "the user's line is still there");
        let mine = remove_block(&compose(&[Block::EscapeBind], &chords()), Block::EscapeBind);
        assert_eq!(mine, format!("{HEADER}\n"));
        assert!(is_empty(&mine));
    }

    #[test]
    fn the_bind_and_the_option_are_the_documented_lua() {
        let t = compose(&[Block::EscapeBind, Block::ReleasePressed], &chords());
        assert!(t.contains(
            r#"hl.bind("SUPER + CTRL + ESCAPE", hl.dsp.release_input_capture(), { allow_input_capture = true, description = "Ferry: release input capture" })"#
        ));
        assert!(t.contains(
            "hl.config({ input = { virtualkeyboard = { release_pressed_on_close = true } } })"
        ));
        let perm = Block::Permission.text(&chords());
        assert!(perm.contains("only when it starts"), "{perm}");
        assert!(perm.contains("answer Hyprland's dialog"), "{perm}");
        assert!(!perm.contains("not be enough"), "{perm}");
        assert!(perm.contains(
            r#"hl.permission({ binary = "/usr/lib/xdg-desktop-portal-hyprland", type = "input-capture", mode = "allow" })"#
        ));
    }

    #[test]
    fn a_foreign_file_is_not_ours() {
        assert!(!is_ours("hl.config({})\n"));
        assert!(!is_ours(""));
    }

    #[test]
    fn the_require_line_is_appended_and_stripped_exactly() {
        for orig in [
            "a = 1\n",
            "a = 1",
            "",
            "a = 1\n\n",
            "require(\"x\")\n-- tail\n",
        ] {
            let (new, added) = append_require(orig);
            assert!(new.ends_with(&added) && requires_ferry(&new));
            assert_eq!(strip_require(&new).as_deref(), Some(orig), "{orig:?}");
        }
        assert_eq!(strip_require("a = 1\n"), None);
    }

    #[test]
    fn a_commented_require_does_not_count() {
        assert!(!requires_ferry("-- require(\"hypr.ferry\")\n"));
        assert!(requires_ferry("require(\"hypr.ferry\") -- mine\n"));
        assert!(requires_ferry("  require('hypr.ferry')\n"));
    }

    use proptest::prelude::*;

    fn any_block() -> impl Strategy<Value = Block> {
        proptest::sample::select(vec![
            Block::EscapeBind,
            Block::ReleasePressed,
            Block::Permission,
        ])
    }

    /// Text that looks like a user's file: some lines of printable text, none of them a ferry
    /// marker. (`ferry.lua` always has its header, so an empty file is not a case.)
    fn user_lines() -> impl Strategy<Value = String> {
        proptest::collection::vec("[ -~]{0,30}", 1..8).prop_map(|lines| {
            lines
                .into_iter()
                .map(|l| format!("{}\n", l.replace("ferry:", "ferry;")))
                .collect()
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// `hyprland.lua` is the user's file: no text, however odd, makes an edit panic.
        #[test]
        fn the_lua_edits_never_panic(text in "\\PC{0,300}", b in any_block()) {
            let _ = (is_ours(&text), has_block(&text, b), is_empty(&text), requires_ferry(&text));
            let _ = remove_block(&text, b);
            let _ = add_block(&text, b, &chords());
            let _ = append_require(&text);
            let _ = strip_require(&text);
        }

        /// `strip_require` "undoes `append_require` exactly", for any text.
        #[test]
        fn strip_require_undoes_append_require(text in "\\PC{0,200}") {
            let (appended, added) = append_require(&text);
            prop_assert!(appended.ends_with(&added));
            prop_assert_eq!(strip_require(&appended), Some(text));
        }

        /// A block added to a file is found, and removing it leaves the user's lines as they
        /// were.
        #[test]
        fn remove_block_undoes_add_block(text in user_lines(), b in any_block()) {
            let added = add_block(&text, b, &chords());
            prop_assert!(has_block(&added, b));
            let removed = remove_block(&added, b);
            prop_assert!(!has_block(&removed, b));
            prop_assert_eq!(removed, text);
        }
    }
}
