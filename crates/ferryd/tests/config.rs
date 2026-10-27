//! `config.schema`, `config.get`, `config.set` and `peer.rename` against an in-process daemon
//! (m3 design §2.6–2.7).
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use ferry_core::api::ConfigSchema;
use ferryd::config::{Config, KEYS};
use serde_json::{Value, json};
use support::*;

const QUIET: Duration = Duration::from_millis(250);

const FILE: &str = r#"# my ferry settings

[peer]
name = "desk"          # what the others call me
port = 4243

[crossing]
# the pointer is hidden while the user pushes
edge_resistance_px = 0   # 0 to 200
escape_chord = "Super+Ctrl+Escape"

[safety]
lock_check_budget_ms = 50
"#;

fn write_config(env: &Env, text: &str) {
    let path = env.paths().config_file;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn config_text(env: &Env) -> String {
    std::fs::read_to_string(env.paths().config_file).unwrap()
}

#[test]
fn the_schema_lists_every_key_of_the_config_with_its_default() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let schema: ConfigSchema =
        serde_json::from_value(c.result("config.schema", json!(null))).unwrap();
    // the keys of the Config struct, no more and no fewer
    let defaults = serde_json::to_value(Config::default()).unwrap();
    let mut fields = Vec::new();
    for (section, t) in defaults.as_object().unwrap() {
        for (name, v) in t.as_object().unwrap() {
            fields.push((format!("{section}.{name}"), v.clone()));
        }
    }
    let mut listed: Vec<_> = schema
        .keys
        .iter()
        .map(|k| (k.key.clone(), k.default.clone()))
        .collect();
    fields.sort_by(|a, b| a.0.cmp(&b.0));
    listed.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(listed, fields);
    assert_eq!(schema.keys.len(), KEYS.len());

    // The schema says what the loader enforces, not what an example in the design note said: a
    // settings view must not offer a value the loader refuses.
    let edge = schema
        .keys
        .iter()
        .find(|k| k.key == "crossing.edge_resistance_px")
        .unwrap();
    assert_eq!((edge.min, edge.max), (Some(0), Some(200)));
    let (code, _, hint) = c.error(
        "config.set",
        json!({"key": "crossing.edge_resistance_px", "value": 201}),
    );
    assert_eq!(code, "bad_value");
    assert!(
        hint.unwrap().contains("0 to 200"),
        "the hint is the range the schema gives"
    );
    c.result(
        "config.set",
        json!({"key": "crossing.edge_resistance_px", "value": 200}),
    );
    d.shutdown();
}

#[test]
fn get_answers_one_key_or_all_and_saves_nothing_it_was_not_told() {
    let env = Env::new();
    write_config(&env, FILE);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let all = c.result("config.get", json!({}));
    assert_eq!(all["values"]["peer.name"], "desk");
    assert_eq!(all["values"]["crossing.escape_chord"], "Super+Ctrl+Escape");
    assert_eq!(
        all["values"]["input.capture"], "auto",
        "a key the file lacks has its default"
    );
    assert_eq!(all["values"].as_object().unwrap().len(), KEYS.len());
    assert!(all.get("pending").is_none());

    let one = c.result("config.get", json!({"key": "peer.port"}));
    assert_eq!(one, json!({"values": {"peer.port": 4243}}));
    assert_eq!(config_text(&env), FILE, "a read writes nothing");

    let (code, message, hint) = c.error("config.get", json!({"key": "peer.nope"}));
    assert_eq!(code, "unknown_key");
    assert!(message.contains("peer.nope"));
    assert!(hint.unwrap().contains("ferry config schema"));
    // a key without its section is a near miss ferryd can name
    let (_, _, hint) = c.error("config.get", json!({"key": "edge_resistance_px"}));
    assert!(hint.unwrap().contains("crossing.edge_resistance_px"));
    d.shutdown();
}

#[test]
fn a_live_key_is_saved_with_the_comments_kept_and_in_force_at_once() {
    let env = Env::new();
    write_config(&env, FILE);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let r = c.result(
        "config.set",
        json!({"key": "crossing.edge_resistance_px", "value": 40}),
    );
    assert_eq!(
        r,
        json!({"key": "crossing.edge_resistance_px", "value": 40, "applies": "live"})
    );
    // only that value changed; the comment after it and every other line stayed
    assert_eq!(
        config_text(&env),
        FILE.replace("edge_resistance_px = 0   #", "edge_resistance_px = 40   #")
    );
    let got = c.result("config.get", json!({"key": "crossing.edge_resistance_px"}));
    assert_eq!(got, json!({"values": {"crossing.edge_resistance_px": 40}}));
    assert!(got.get("pending").is_none(), "a live key is in force");
    d.shutdown();

    // and it is what the next start reads
    let d = Daemon::start(options(&env)).unwrap();
    let mut c = Client::connect(d.socket_path());
    let got = c.result("config.get", json!({"key": "crossing.edge_resistance_px"}));
    assert_eq!(got["values"]["crossing.edge_resistance_px"], 40);
    d.shutdown();
}

use ferryd::Daemon;

#[test]
fn the_escape_chord_is_matched_at_once_and_the_status_shows_it() {
    let env = Env::new();
    write_config(&env, FILE);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    assert_eq!(
        c.result("status", json!(null))["escape_chord"],
        "Super+Ctrl+Escape"
    );
    let r = c.result(
        "config.set",
        json!({"key": "crossing.escape_chord", "value": "ctrl+alt+f12"}),
    );
    // the spelling is the canonical one, and ferry setup still has the bind to update
    assert_eq!(r["value"], "Ctrl+Alt+F12");
    assert_eq!(r["applies"], "setup");
    assert_eq!(
        c.result("status", json!(null))["escape_chord"],
        "Ctrl+Alt+F12"
    );
    assert!(config_text(&env).contains("escape_chord = \"Ctrl+Alt+F12\""));
    d.shutdown();
}

#[test]
fn a_restart_key_is_saved_and_pending_until_ferryd_restarts() {
    let env = Env::new();
    write_config(&env, FILE);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let port = c.result("status", json!(null))["port"].clone();
    let r = c.result("config.set", json!({"key": "peer.port", "value": 4300}));
    assert_eq!(r["applies"], "restart");
    assert!(config_text(&env).contains("port = 4300"));

    // the saved value, marked as waiting
    let got = c.result("config.get", json!({}));
    assert_eq!(got["values"]["peer.port"], 4300);
    assert_eq!(got["pending"], json!(["peer.port"]));
    assert_eq!(
        c.result("config.get", json!({"key": "peer.port"}))["pending"],
        json!(["peer.port"])
    );
    // the running daemon is untouched
    assert_eq!(c.result("status", json!(null))["port"], port);
    d.shutdown();

    let d = Daemon::start(options(&env)).unwrap();
    let mut c = Client::connect(d.socket_path());
    let got = c.result("config.get", json!({}));
    assert_eq!(got["values"]["peer.port"], 4300);
    assert!(got.get("pending").is_none(), "in force after the restart");
    d.shutdown();
}

#[test]
fn a_refused_value_changes_nothing_and_says_what_is_allowed() {
    let env = Env::new();
    write_config(&env, FILE);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let before = c.result("config.get", json!({}));
    let cases = [
        (
            "crossing.edge_resistance_px",
            json!(900),
            "bad_value",
            "0 to 200 px",
        ),
        (
            "crossing.edge_resistance_px",
            json!("40"),
            "bad_value",
            "0 to 200 px",
        ),
        (
            "crossing.edge_resistance_px",
            json!(1.5),
            "bad_value",
            "0 to 200 px",
        ),
        (
            "safety.lock_check_budget_ms",
            json!(10),
            "bad_value",
            "50 to 100 ms",
        ),
        (
            "input.capture",
            json!("telepathy"),
            "bad_value",
            "one of auto",
        ),
        (
            "crossing.escape_chord",
            json!("Escape"),
            "bad_value",
            "two modifiers",
        ),
        (
            "peer.name",
            json!("two words"),
            "bad_value",
            "no whitespace",
        ),
        ("peer.name", json!(42), "bad_value", "no whitespace"),
        (
            "crossing.block_while_button_held",
            json!("yes"),
            "bad_value",
            "true or false",
        ),
        ("peer.nope", json!(1), "unknown_key", "ferry config schema"),
    ];
    for (key, value, code, hint) in cases {
        let (got, message, h) = c.error("config.set", json!({"key": key, "value": value}));
        assert_eq!(got, code, "{key} = {value}: {message}");
        assert!(message.contains(key), "{message}");
        assert!(h.unwrap().contains(hint), "{key} = {value}");
    }
    assert_eq!(c.result("config.get", json!({})), before);
    assert_eq!(config_text(&env), FILE, "no refusal touched the file");
    // a request of the wrong shape is bad_params
    assert_eq!(
        c.error("config.set", json!({"key": "peer.port"})).0,
        "bad_params"
    );
    assert_eq!(c.error("config.set", json!(null)).0, "bad_params");
    d.shutdown();
}

#[test]
fn a_file_that_is_missing_or_lacks_the_section_is_made_to_say_it() {
    let env = Env::new();
    let d = start(&env);
    let path = env.paths().config_file;
    assert!(!path.exists());
    let mut c = Client::connect(d.socket_path());
    c.result(
        "config.set",
        json!({"key": "crossing.edge_dwell_ms", "value": 120}),
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[crossing]\nedge_dwell_ms = 120\n"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
        0o644
    );
    // nothing else was left in the directory
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");

    c.result("config.set", json!({"key": "peer.name", "value": "lap"}));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[crossing]\nedge_dwell_ms = 120\n\n[peer]\nname = \"lap\"\n"
    );
    d.shutdown();
}

#[test]
fn the_files_mode_and_a_link_to_it_are_kept() {
    let env = Env::new();
    // the user keeps the file in a dotfiles repository and links it
    let real = env.tmp.path().join("dotfiles/ferry.toml");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, FILE).unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = env.paths().config_file;
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result(
        "config.set",
        json!({"key": "crossing.edge_resistance_px", "value": 7}),
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link survives"
    );
    assert!(
        std::fs::read_to_string(&real)
            .unwrap()
            .contains("edge_resistance_px = 7")
    );
    assert_eq!(
        std::fs::metadata(&real).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    d.shutdown();
}

#[test]
fn a_file_that_cannot_be_written_is_read_only_and_nothing_changes() {
    if rustix::process::getuid().is_root() {
        return; // root writes anywhere
    }
    let env = Env::new();
    write_config(&env, FILE);
    let dir = env.paths().config_file.parent().unwrap().to_path_buf();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (code, message, hint) = c.error(
        "config.set",
        json!({"key": "crossing.edge_resistance_px", "value": 40}),
    );
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(code, "read_only", "{message}");
    assert!(hint.unwrap().contains("nothing was changed"));
    assert_eq!(config_text(&env), FILE);
    assert_eq!(
        c.result("config.get", json!({"key": "crossing.edge_resistance_px"}))["values"]["crossing.edge_resistance_px"],
        0
    );
    // writable again: it works
    c.result(
        "config.set",
        json!({"key": "crossing.edge_resistance_px", "value": 40}),
    );
    d.shutdown();
}

#[test]
fn a_file_ferryd_cannot_read_is_not_overwritten() {
    let env = Env::new();
    write_config(&env, FILE);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    // edited by hand since ferryd started, and broken
    let broken = "[crossing\nedge_resistance_px = \n";
    std::fs::write(env.paths().config_file, broken).unwrap();
    let (code, message, hint) = c.error(
        "config.set",
        json!({"key": "crossing.edge_resistance_px", "value": 40}),
    );
    assert_eq!(code, "internal", "{message}");
    assert!(hint.unwrap().contains("fix the file by hand"));
    assert_eq!(config_text(&env), broken);
    d.shutdown();
}

#[test]
fn a_set_is_an_event_to_the_config_subscribers_only_when_it_changes_something() {
    let env = Env::new();
    write_config(&env, FILE);
    let d = start(&env);
    let mut watcher = Client::connect(d.socket_path());
    watcher.subscribe(&["config"]);
    let first = watcher
        .event_within("config", Duration::from_secs(5))
        .unwrap();
    assert_eq!(first["data"]["values"]["crossing.edge_resistance_px"], 0);

    let mut c = Client::connect(d.socket_path());
    c.result(
        "config.set",
        json!({"key": "crossing.edge_resistance_px", "value": 40}),
    );
    let ev = watcher
        .event_within("config", Duration::from_secs(5))
        .unwrap();
    assert_eq!(ev["data"]["values"]["crossing.edge_resistance_px"], 40);
    // the payload is what `config.get` answers, `{"values": {...}}`
    assert_eq!(ev["data"], c.result("config.get", json!(null)));
    assert!(ev["data"]["values"].is_object());

    // the same value again is not news; the next real change is, and arrives next
    c.result(
        "config.set",
        json!({"key": "crossing.edge_resistance_px", "value": 40}),
    );
    c.result("config.set", json!({"key": "peer.port", "value": 4300}));
    let ev = watcher
        .event_within("config", Duration::from_secs(5))
        .unwrap();
    assert_eq!(ev["data"]["values"]["peer.port"], 4300);
    assert_eq!(ev["data"]["pending"], json!(["peer.port"]));
    assert!(watcher.lines_until_quiet(QUIET).is_empty());
    d.shutdown();
}

#[test]
fn a_problem_in_the_file_is_attention_and_the_status_says_so() {
    let env = Env::new();
    write_config(
        &env,
        "[peer]\nnmae = \"typo\"\n[safety]\nlock_check_budget_ms = 250\n",
    );
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let st = c.result("status", json!(null));
    assert_eq!(st["summary"], json!({"state": "attention"}));
    let a = &st["attention"][0];
    assert_eq!(a["code"], "config_invalid");
    assert!(a["message"].as_str().unwrap().contains("2 problems"), "{a}");
    assert!(a["message"].as_str().unwrap().contains("peer.nmae"), "{a}");
    assert!(a["hint"].is_string());
    // the state topic carries it from the first snapshot
    let mut w = Client::connect(d.socket_path());
    w.subscribe(&["state"]);
    let ev = w.event_within("state", Duration::from_secs(5)).unwrap();
    assert_eq!(ev["data"]["attention"][0]["code"], "config_invalid");

    // setting the clamped key writes a value that loads as it is: that line of the problem goes,
    // and the state topic says so; the typo stays until the file is fixed by hand
    c.result(
        "config.set",
        json!({"key": "safety.lock_check_budget_ms", "value": 100}),
    );
    let ev = w.event_within("state", Duration::from_secs(5)).unwrap();
    let a = &ev["data"]["attention"][0];
    assert_eq!(a["message"], "config.toml: unknown key peer.nmae");
    d.shutdown();
}

// --- peer.rename -------------------------------------------------------------------------------

#[test]
fn a_trusted_peer_is_renamed_everywhere_it_is_shown() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "laptop"}));
    c.result("trust", json!({"fingerprint": fpx(0x9d), "name": "other"}));
    let mut w = Client::connect(d.socket_path());
    w.subscribe(&["peers", "layout"]);
    w.lines_until_quiet(QUIET);

    let r = c.result("peer.rename", json!({"peer": "laptop", "name": "netbook"}));
    assert_eq!(
        r,
        json!({"peer": {"name": "netbook", "fingerprint": fpx(0x9c)}})
    );
    assert!(
        std::fs::read_to_string(env.paths().trusted_file())
            .unwrap()
            .contains("name = \"netbook\"")
    );
    let peers = w.event_within("peers", Duration::from_secs(5)).unwrap();
    assert_eq!(peers["data"]["peers"][0]["name"], "netbook");
    let layout = w.event_within("layout", Duration::from_secs(5)).unwrap();
    assert!(layout["data"]["peers"].to_string().contains("netbook"));
    assert_eq!(
        c.result("status", json!(null))["peers"][0]["name"],
        "netbook"
    );
    // by a fingerprint prefix, and to the same name again
    let again = c.result(
        "peer.rename",
        json!({"peer": &fpx(0x9c)[..8], "name": "netbook"}),
    );
    assert_eq!(again["peer"]["name"], "netbook");
    d.shutdown();

    // it survives a restart
    let d = Daemon::start(options(&env)).unwrap();
    let mut c = Client::connect(d.socket_path());
    assert_eq!(
        c.result("peers", json!(null))["peers"][0]["name"],
        "netbook"
    );
    d.shutdown();
}

#[test]
fn a_rename_that_is_refused_changes_nothing() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "laptop"}));
    c.result("trust", json!({"fingerprint": fpx(0x9d), "name": "other"}));
    let before = std::fs::read_to_string(env.paths().trusted_file()).unwrap();
    for (peer, name, code) in [
        ("nobody", "x", "unknown_peer"),
        ("laptop", "two words", "bad_params"),
        ("laptop", "", "bad_params"),
        ("laptop", "a:b", "bad_params"),
        ("laptop", "other", "bad_params"),
        ("laptop", "desk", "bad_params"), // this machine's own name
        ("desk", "x", "bad_params"),      // this machine is not renamed here
    ] {
        let (got, message, _) = c.error("peer.rename", json!({"peer": peer, "name": name}));
        assert_eq!(got, code, "{peer} -> {name:?}: {message}");
    }
    assert_eq!(
        c.error("peer.rename", json!({"peer": "laptop"})).0,
        "bad_params"
    );
    assert_eq!(
        std::fs::read_to_string(env.paths().trusted_file()).unwrap(),
        before
    );
    let st: Value = c.result("status", json!(null));
    assert_eq!(st["peers"][0]["name"], "laptop");
    d.shutdown();
}

/// D-3: what an open Settings panel relies on. A live key set from another connection (the CLI)
/// is an event to the watcher, and a fresh subscription, as after a ferryd restart, starts with
/// the values saved before it.
#[test]
fn a_set_from_another_connection_and_a_resubscribe_carry_the_saved_values() {
    let env = Env::new();
    write_config(&env, FILE);
    let d = start(&env);
    let mut watcher = Client::connect(d.socket_path());
    watcher.subscribe(&["config"]);
    let first = watcher
        .event_within("config", Duration::from_secs(5))
        .unwrap();
    assert_eq!(first["data"]["values"]["notify.enabled"], true);

    let mut cli = Client::connect(d.socket_path());
    cli.result(
        "config.set",
        json!({"key": "notify.enabled", "value": false}),
    );
    let ev = watcher
        .event_within("config", Duration::from_secs(5))
        .unwrap();
    assert_eq!(ev["data"]["values"]["notify.enabled"], false);
    cli.result("config.set", json!({"key": "peer.port", "value": 4244}));
    let ev = watcher
        .event_within("config", Duration::from_secs(5))
        .unwrap();
    assert_eq!(ev["data"]["pending"], json!(["peer.port"]));
    d.shutdown();

    // the restart: the port saved before it is in force, nothing waits, and the first event says so
    let d = start(&env);
    let mut watcher = Client::connect(d.socket_path());
    watcher.subscribe(&["config"]);
    let first = watcher
        .event_within("config", Duration::from_secs(5))
        .unwrap();
    assert_eq!(first["data"]["values"]["notify.enabled"], false);
    assert_eq!(first["data"]["values"]["peer.port"], 4244);
    assert!(
        first["data"].get("pending").is_none_or(|p| p == &json!([])),
        "{first}"
    );
    d.shutdown();
}
