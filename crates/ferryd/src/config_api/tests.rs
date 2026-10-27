//! `config.set` without a daemon: the value rules, and the edit of the file's text.

use proptest::prelude::*;
use serde_json::json;

use super::*;
use crate::config::{CaptureMode, Config, KEYS};

/// Every key present, with the comments a person writes.
const SAMPLE: &str = r#"# ~/.config/ferry/config.toml: edit it by hand or with `ferry config`

[peer]
name = "desk"                 # default: the hostname
port = 4243                   # UDP

[crossing]
# the pointer is hidden while the user pushes (S14)
edge_resistance_px = 0        # 0 to 200
edge_dwell_ms = 0
corner_dead_zone_px = 8
block_while_button_held = true
escape_chord = "Super+Ctrl+Escape"   # needs at least 2 modifiers
go_chord = "Super+Ctrl+Alt"      # modifiers only; an arrow key completes it
edge_glow = true

[input]
capture = "auto"
emulation = "wayland"
scroll_invert = false

[clipboard]
enabled = true
max_bytes = 33554432

[discovery]
mdns = true
advertise = true

[safety]
lock_check_budget_ms = 50     # 50 to 100

[notify]
enabled = true                # desktop toasts; the bar plugin gets them either way
# the end of the file
"#;

fn key(name: &str) -> &'static Key {
    find_key(name).unwrap_or_else(|| panic!("no key {name}"))
}

fn parse(text: &str) -> Config {
    Config::parse(text, Path::new("/x/config.toml")).unwrap().0
}

fn set(text: &str, name: &str, value: Value) -> String {
    edit_toml(text, key(name), &value).unwrap()
}

#[test]
fn the_sample_is_the_defaults() {
    // the rest of this file edits it; it must say what an empty file says
    let mut sample = parse(SAMPLE);
    sample.peer.name.clear();
    assert_eq!(sample, Config::default());
}

// --- the schema --------------------------------------------------------------------------------

/// The key list a frontend renders: type, range, default, when it applies, in file order.
#[test]
fn the_schema_of_every_key() {
    insta::assert_json_snapshot!(schema());
}

#[test]
fn every_default_in_the_schema_is_the_default_config() {
    let defaults = serde_json::to_value(Config::default()).unwrap();
    for k in schema().keys {
        let (section, name) = k.key.split_once('.').unwrap();
        assert_eq!(k.default, defaults[section][name], "{}", k.key);
        assert!(!k.doc.is_empty(), "{}", k.key);
        match k.kind {
            ConfigType::Int => assert!(k.min.is_some() && k.max.is_some(), "{}", k.key),
            ConfigType::Enum => assert!(k.choices.as_ref().is_some_and(|c| !c.is_empty())),
            _ => assert!(k.min.is_none() && k.max.is_none() && k.choices.is_none()),
        }
        // the default of a key is a value the key takes
        let mut c = Config::default();
        apply(&mut c, key(&k.key), &k.default).unwrap_or_else(|e| panic!("{}: {e:?}", k.key));
    }
}

#[test]
fn the_choices_are_the_names_the_file_loader_reads() {
    for mode in CaptureMode::ALL {
        let c = parse(&format!("[input]\ncapture = \"{}\"\n", mode.as_str()));
        assert_eq!(c.input.capture, mode);
    }
    let Field::Enum { choices, .. } = key("input.capture").field else {
        panic!("not an enum");
    };
    let listed: Vec<_> = CaptureMode::ALL.iter().map(|m| m.as_str()).collect();
    assert_eq!(choices, listed.as_slice());
}

// --- the value rules ---------------------------------------------------------------------------

#[test]
fn a_value_of_the_right_type_in_range_is_applied_and_returned_as_saved() {
    let mut c = Config::default();
    let r = |c: &mut Config, k: &str, v: Value| apply(c, key(k), &v).unwrap();
    assert_eq!(
        r(&mut c, "crossing.edge_resistance_px", json!(40)),
        json!(40)
    );
    assert_eq!(c.crossing.edge_resistance_px, 40);
    assert_eq!(
        r(&mut c, "crossing.block_while_button_held", json!(false)),
        json!(false)
    );
    assert!(!c.crossing.block_while_button_held);
    assert_eq!(r(&mut c, "peer.name", json!("lap")), json!("lap"));
    assert_eq!(c.peer.name, "lap");
    assert_eq!(r(&mut c, "input.capture", json!("off")), json!("off"));
    assert_eq!(c.input.capture, CaptureMode::Off);
    assert_eq!(
        r(&mut c, "clipboard.max_bytes", json!(1 << 20)),
        json!(1 << 20)
    );
    assert_eq!(c.clipboard.max_bytes, 1 << 20);
    // the chord is saved in the spelling the status shows
    assert_eq!(
        r(&mut c, "crossing.escape_chord", json!("ctrl+alt+f12")),
        json!("Ctrl+Alt+F12")
    );
    assert_eq!(c.crossing.escape_chord, "Ctrl+Alt+F12");
    // the ends of a range are in it
    assert_eq!(
        r(&mut c, "safety.lock_check_budget_ms", json!(100)),
        json!(100)
    );
    assert_eq!(r(&mut c, "peer.port", json!(65535)), json!(65535));
    assert_eq!(c.peer.port, 65535);
}

#[test]
fn every_refusal_is_bad_value_with_a_hint_that_says_what_is_allowed() {
    let cases = [
        (
            "crossing.edge_resistance_px",
            json!(201),
            "out of range",
            "0 to 200 px",
        ),
        (
            "crossing.edge_resistance_px",
            json!(-1),
            "out of range",
            "0 to 200 px",
        ),
        (
            "crossing.edge_resistance_px",
            json!(4.5),
            "whole number",
            "0 to 200 px",
        ),
        (
            "crossing.edge_resistance_px",
            json!("40"),
            "whole number",
            "0 to 200 px",
        ),
        (
            "crossing.edge_resistance_px",
            json!(true),
            "whole number",
            "0 to 200 px",
        ),
        (
            "crossing.edge_resistance_px",
            json!(null),
            "whole number",
            "0 to 200 px",
        ),
        (
            "crossing.edge_resistance_px",
            json!(u64::MAX),
            "whole number",
            "0 to 200 px",
        ),
        (
            "safety.lock_check_budget_ms",
            json!(250),
            "out of range",
            "50 to 100 ms",
        ),
        ("peer.port", json!(65536), "out of range", "0 to 65535"),
        (
            "clipboard.max_bytes",
            json!(-5),
            "out of range",
            "0 to 9223372036854775807 bytes",
        ),
        (
            "crossing.block_while_button_held",
            json!("yes"),
            "true or false",
            "true or false",
        ),
        (
            "crossing.block_while_button_held",
            json!(1),
            "true or false",
            "true or false",
        ),
        (
            "input.capture",
            json!("telepathy"),
            "not one of the choices",
            "one of auto, portal",
        ),
        (
            "input.capture",
            json!(3),
            "expected a text",
            "one of auto, portal",
        ),
        (
            "input.emulation",
            json!("uinput"),
            "not one of the choices",
            "one of wayland",
        ),
        (
            "crossing.escape_chord",
            json!("Ctrl+Escape"),
            "2 modifiers",
            "at least two modifiers",
        ),
        (
            "crossing.escape_chord",
            json!("Super+Ctrl+Nope"),
            "Nope",
            "at least two modifiers",
        ),
        (
            "crossing.escape_chord",
            json!(5),
            "expected a text",
            "at least two modifiers",
        ),
        ("peer.name", json!(5), "expected a text", "no whitespace"),
    ];
    for (k, value, message, hint) in cases {
        let mut c = Config::default();
        let before = c.clone();
        let e = apply(&mut c, key(k), &value).expect_err(&format!("{k} = {value}"));
        assert_eq!(e.code, ErrorCode::BadValue, "{k} = {value}");
        assert!(e.message.contains(k), "{}", e.message);
        assert!(e.message.contains(message), "{k} = {value}: {}", e.message);
        assert!(
            e.hint.as_deref().is_some_and(|h| h.contains(hint)),
            "{k} = {value}: {:?}",
            e.hint
        );
        assert_eq!(c, before, "a refusal changes nothing");
    }
}

/// Names are checked by the loader's own rule, which `validate` applies to the result.
#[test]
fn a_name_the_loader_would_refuse_is_refused_by_validate_with_its_words() {
    for bad in ["a b", "a:b", "a\tb", &"x".repeat(65)] {
        let mut c = Config::default();
        apply(&mut c, key("peer.name"), &json!(bad)).unwrap();
        let e = c.validate().unwrap_err();
        assert!(e.starts_with("[peer] name:"), "{e}");
        let (k, why) = c.refusal().unwrap();
        assert_eq!(k.key, "peer.name");
        assert!(e.ends_with(&why), "{e}");
    }
    // empty is the hostname
    let mut c = Config::default();
    apply(&mut c, key("peer.name"), &json!("")).unwrap();
    assert!(c.validate().is_ok());
}

// --- the text of the file ----------------------------------------------------------------------

#[test]
fn a_set_value_replaces_the_old_one_and_the_comment_after_it_stays() {
    let out = set(SAMPLE, "crossing.edge_resistance_px", json!(40));
    assert_eq!(
        out,
        SAMPLE.replace(
            "edge_resistance_px = 0        # 0 to 200",
            "edge_resistance_px = 40        # 0 to 200"
        )
    );
    // a string keeps its comment too
    let out = set(SAMPLE, "crossing.escape_chord", json!("Super+Alt+F12"));
    assert!(
        out.contains("escape_chord = \"Super+Alt+F12\"   # needs at least 2 modifiers"),
        "{out}"
    );
    let out = set(SAMPLE, "peer.name", json!("lap"));
    assert!(
        out.contains("name = \"lap\"                 # default: the hostname"),
        "{out}"
    );
    let out = set(SAMPLE, "crossing.block_while_button_held", json!(false));
    assert!(out.contains("block_while_button_held = false\n"), "{out}");
}

#[test]
fn only_the_line_of_the_key_changes() {
    for k in KEYS {
        let value = k.value(&Config::default());
        // the same value written again, and a different one
        for v in [value.clone(), other_value(k)] {
            let out = set(SAMPLE, k.key, v);
            let (before, after): (Vec<_>, Vec<_>) =
                (SAMPLE.lines().collect(), out.lines().collect());
            assert_eq!(before.len(), after.len(), "{}", k.key);
            let name = k.parts().1;
            for (a, b) in before.iter().zip(&after) {
                if a.starts_with(&format!("{name} = ")) {
                    assert!(b.starts_with(&format!("{name} = ")), "{a} -> {b}");
                    assert_eq!(
                        a.split_once('#').map(|c| c.1),
                        b.split_once('#').map(|c| c.1)
                    );
                } else {
                    assert_eq!(a, b, "{}", k.key);
                }
            }
        }
    }
}

/// A value of the key's type that is not the default.
fn other_value(k: &Key) -> Value {
    match k.field {
        Field::Bool { get, .. } => json!(!get(&Config::default())),
        Field::Int { min, max, .. } => json!(if max > min { max } else { min }),
        Field::Name { .. } => json!("other"),
        Field::Chord {
            kind: ChordKind::Escape,
            ..
        } => json!("Ctrl+Alt+F12"),
        Field::Chord {
            kind: ChordKind::Go,
            ..
        } => json!("Ctrl+Shift"),
        Field::Enum { choices, .. } => json!(choices.last().unwrap()),
    }
}

#[test]
fn a_key_the_file_lacks_is_added_to_its_section() {
    let text =
        "[crossing]\n# dead corners\ncorner_dead_zone_px = 8\n\n[input]\ncapture = \"auto\"\n";
    let out = set(text, "crossing.edge_resistance_px", json!(40));
    assert_eq!(
        out,
        "[crossing]\n# dead corners\ncorner_dead_zone_px = 8\nedge_resistance_px = 40\n\n[input]\ncapture = \"auto\"\n"
    );
    assert_eq!(parse(&out).crossing.edge_resistance_px, 40);
}

#[test]
fn a_section_the_file_lacks_is_added_at_the_end() {
    let text = "[peer]\nname = \"desk\"  # mine\n";
    let out = set(text, "crossing.edge_resistance_px", json!(40));
    assert_eq!(
        out,
        "[peer]\nname = \"desk\"  # mine\n\n[crossing]\nedge_resistance_px = 40\n"
    );
    // an empty file, and no file at all, give just the section
    let out = set("", "crossing.edge_resistance_px", json!(40));
    assert_eq!(out, "[crossing]\nedge_resistance_px = 40\n");
    assert_eq!(
        set("\n\n", "peer.port", json!(4300)).trim(),
        "[peer]\nport = 4300"
    );
}

#[test]
fn a_section_written_as_an_inline_table_is_edited_in_place() {
    let text = "crossing = { edge_resistance_px = 5, corner_dead_zone_px = 8 }\n";
    let out = set(text, "crossing.edge_resistance_px", json!(40));
    assert_eq!(
        out,
        "crossing = { edge_resistance_px = 40, corner_dead_zone_px = 8 }\n"
    );
    let c = parse(&out);
    assert_eq!(
        (
            c.crossing.edge_resistance_px,
            c.crossing.corner_dead_zone_px
        ),
        (40, 8)
    );
    // and a key it lacks
    let out = set(text, "crossing.edge_dwell_ms", json!(100));
    assert_eq!(parse(&out).crossing.edge_dwell_ms, 100);
}

#[test]
fn a_file_that_is_not_toml_or_has_no_table_is_not_overwritten() {
    for text in ["[crossing\n", "crossing = 5\n", "[peer]\nport = \n"] {
        let e = edit_toml(text, key("crossing.edge_resistance_px"), &json!(40)).unwrap_err();
        assert_eq!(e.code, ErrorCode::Internal, "{text:?}");
        assert!(e.hint.as_deref().unwrap().contains("fix the file by hand"));
    }
}

#[test]
fn text_with_quotes_and_unicode_is_written_so_it_reads_back() {
    for name in ["a\"b", "a\\b", "näme", "x'y", "tab\u{2003}x"] {
        // (the last has a space character; the loader refuses it, the text edit does not care)
        let out = set(SAMPLE, "peer.name", json!(name));
        let back: toml::Table = out.parse().unwrap();
        assert_eq!(back["peer"]["name"].as_str(), Some(name), "{out}");
    }
}

#[test]
fn a_value_that_toml_cannot_hold_is_refused_before_it_is_written() {
    for v in [json!(null), json!([1]), json!({"a": 1}), json!(1.5)] {
        let e = edit_toml(SAMPLE, key("peer.port"), &v).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadValue, "{v}");
    }
}

// --- the round trip ----------------------------------------------------------------------------

fn arb_value(k: &'static Key) -> BoxedStrategy<Value> {
    match k.field {
        Field::Bool { .. } => any::<bool>().prop_map(Value::Bool).boxed(),
        Field::Int { min, max, .. } => (min..=max).prop_map(Value::from).boxed(),
        Field::Name { .. } => "[a-zA-Z0-9._-]{0,20}".prop_map(Value::from).boxed(),
        Field::Chord {
            kind: ChordKind::Escape,
            ..
        } => proptest::sample::select(vec![
            "Super+Ctrl+Escape",
            "Super+Alt+F12",
            "Ctrl+Alt+Delete",
            "ctrl+shift+q",
            "Super+Shift+Alt+Backspace",
        ])
        .prop_map(Value::from)
        .boxed(),
        Field::Chord {
            kind: ChordKind::Go,
            ..
        } => proptest::sample::select(vec!["Super+Ctrl+Alt", "Ctrl+Shift", "super+alt"])
            .prop_map(Value::from)
            .boxed(),
        Field::Enum { choices, .. } => proptest::sample::select(choices.to_vec())
            .prop_map(Value::from)
            .boxed(),
    }
}

fn arb_edit() -> impl Strategy<Value = (&'static Key, Value)> {
    proptest::sample::select(KEYS.iter().collect::<Vec<_>>())
        .prop_flat_map(|k| (Just(k), arb_value(k)))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Any run of valid settings, applied to a config and to the text of the file in step: the
    /// file reads back as the config says (no warning), and the lines of the keys nobody set,
    /// the comments and the order stay exactly as they were.
    #[test]
    fn a_run_of_sets_keeps_the_file_and_the_config_in_step(edits in proptest::collection::vec(arb_edit(), 1..12)) {
        let mut config = parse(SAMPLE);
        let mut text = SAMPLE.to_string();
        for (k, value) in &edits {
            let saved = apply(&mut config, k, value).unwrap();
            prop_assert!(config.validate().is_ok());
            text = edit_toml(&text, k, &saved).unwrap();
            let (read, warnings) = Config::parse(&text, Path::new("/x/config.toml")).unwrap();
            prop_assert_eq!(&read, &config, "after {} = {}", k.key, saved);
            prop_assert!(warnings.is_empty(), "{warnings:?}");
        }
        // the comment lines are all still there, in order
        let comments = |t: &str| -> Vec<String> {
            t.lines()
                .filter(|l| l.trim_start().starts_with('#'))
                .map(str::to_string)
                .collect()
        };
        prop_assert_eq!(comments(&text), comments(SAMPLE));
        // so are the trailing comments of the lines
        let trailing = |t: &str| -> Vec<String> {
            t.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .filter_map(|l| l.split_once('#').map(|c| c.1.to_string()))
                .collect()
        };
        prop_assert_eq!(trailing(&text), trailing(SAMPLE));
        // and the number of lines is the number it was
        prop_assert_eq!(text.lines().count(), SAMPLE.lines().count());
    }

    /// From a file with only some keys, or none, the same holds for the values.
    #[test]
    fn sets_on_a_sparse_file_read_back(
        edits in proptest::collection::vec(arb_edit(), 1..12),
        start in proptest::sample::select(vec!["", "# only a comment\n", "[peer]\nname = \"x\"\n", "[crossing]\nescape_chord = \"Super+Ctrl+Escape\" # keep\n"]),
    ) {
        let mut config = parse(start);
        let mut text = start.to_string();
        for (k, value) in &edits {
            let saved = apply(&mut config, k, value).unwrap();
            text = edit_toml(&text, k, &saved).unwrap();
            let (read, _) = Config::parse(&text, Path::new("/x/config.toml")).unwrap();
            prop_assert_eq!(&read, &config, "after {} = {}\n{}", k.key, saved, text);
        }
        // a comment stays, on the line of the key whose value changed or not
        for comment in start.lines().filter_map(|l| l.split_once('#').map(|c| c.1)) {
            prop_assert!(text.contains(comment), "{comment:?} lost from\n{text}");
        }
    }

    /// Whatever JSON arrives for whatever key, `apply` answers a value or `bad_value`; it never
    /// panics, and a refusal changes nothing.
    #[test]
    fn apply_never_panics_and_a_refusal_changes_nothing(
        k in proptest::sample::select(KEYS.iter().collect::<Vec<_>>()),
        value in prop_oneof![
            Just(json!(null)),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::from),
            any::<u64>().prop_map(Value::from),
            any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(|f| json!(f)),
            "\\PC{0,24}".prop_map(Value::from),
            Just(json!([1, 2])),
            Just(json!({"a": 1})),
        ],
    ) {
        let mut c = Config::default();
        match apply(&mut c, k, &value) {
            Ok(saved) => {
                // what it says it saved is what the config now holds
                prop_assert_eq!(k.value(&c), saved);
            }
            Err(e) => {
                prop_assert_eq!(e.code, ErrorCode::BadValue);
                prop_assert_eq!(c, Config::default());
            }
        }
    }
}
