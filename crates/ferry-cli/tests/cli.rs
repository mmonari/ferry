//! The `ferry` CLI against an in-process daemon: output, `--json`, errors and exit codes.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use ferry_cli::run;
use serde_json::{Value, json};
use support::*;

const OWN_FP_LEN: usize = 64;

fn ferry(env: &Env, args: &[&str]) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(
        std::iter::once("ferry").chain(args.iter().copied()),
        &env.socket(),
        &mut out,
        &mut err,
    );
    (code, text(&out), text(&err))
}

#[test]
fn status_human_and_json() {
    let env = Env::new();
    let d = env.daemon("desk");
    let (code, out, err) = ferry(&env, &["status"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.starts_with("desk (this machine)\n"), "{out}");
    assert!(out.contains(&d.fingerprint().to_string()));
    assert!(out.contains("No peers."));

    let (code, out, _) = ferry(&env, &["status", "--json"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["api"], 1);
    assert_eq!(v["me"]["name"], "desk");
    assert_eq!(out.lines().count(), 1, "--json is one line");
    d.shutdown();
}

#[test]
fn id_prints_the_paste_line() {
    let env = Env::new();
    let d = env.daemon("desk");
    let (code, out, _) = ferry(&env, &["id"]);
    assert_eq!(code, 0);
    let fp = d.fingerprint().to_string();
    assert!(out.contains(&format!("fingerprint  {fp}")), "{out}");
    let line = out
        .lines()
        .find(|l| l.trim_start().starts_with("ferry trust"))
        .expect("a paste line")
        .trim();
    assert!(
        line.starts_with(&format!("ferry trust {fp} --name desk")),
        "{line}"
    );
    if line.contains("--addr") {
        assert!(
            line.ends_with(&format!(":{}", d.net_addr().port())),
            "{line}"
        );
    }
    let v: Value = serde_json::from_str(&ferry(&env, &["--json", "id"]).1).unwrap();
    assert_eq!(v["fingerprint"], fp);
    assert_eq!(v["port"], d.net_addr().port());
    d.shutdown();
}

#[test]
fn trust_pins_and_a_repeat_is_fine() {
    let env = Env::new();
    let d = env.daemon("desk");
    let fp = "9c".repeat(32);
    let pasted = format!("SHA256:{}", fp.to_uppercase());
    for _ in 0..2 {
        let (code, out, err) = ferry(&env, &["trust", &pasted, "--name", "laptop"]);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.starts_with(&format!("Trusted laptop ({fp})")), "{out}");
    }
    let v: Value = serde_json::from_str(&ferry(&env, &["trust", &fp, "--json"]).1).unwrap();
    assert_eq!(v, json!({"peer": {"name": "laptop", "fingerprint": fp}}));
    let (_, out, _) = ferry(&env, &["status"]);
    assert!(out.contains("laptop"), "{out}");
    d.shutdown();
}

#[test]
fn daemon_errors_are_exit_1_with_the_message_and_hint() {
    let env = Env::new();
    let d = env.daemon("desk");
    let own = d.fingerprint().to_string();
    let (code, out, err) = ferry(&env, &["trust", &own]);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(
        err.starts_with("error: that is this machine's own fingerprint"),
        "{err}"
    );
    assert!(
        err.contains("hint: run `ferry id` on the other machine"),
        "{err}"
    );

    let (code, _, err) = ferry(&env, &["layout", "place", "nobody", "--below", "HDMI-A-1"]);
    assert_eq!(code, 1);
    assert!(err.contains("no peer is named \"nobody\""), "{err}");

    let (code, _, err) = ferry(&env, &["trust", &"ab".repeat(32), "--addr", "not an addr"]);
    assert_eq!(code, 1);
    assert!(err.contains("addr"), "{err}");
    d.shutdown();
}

#[test]
fn usage_errors_are_exit_2_and_never_touch_the_daemon() {
    let env = Env::new(); // no daemon at all: a usage error is found first
    for args in [
        &["trust", "tooshort"][..],
        &["nonsense"],
        &["layout", "place", "m"],
        &["layout", "set", "m", "1"],
        &["layout", "set", "m", "1", "two"],
        &["layout", "set", "m", "1.5", "2"],
        &["layout", "place", "m", "--below", "a", "--above", "b"],
        &[
            "layout", "place", "m", "--below", "a", "--align", "sideways",
        ],
        &["status", "--wat"],
    ] {
        let (code, out, err) = ferry(&env, args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert_eq!(out, "");
        assert!(err.contains("Usage") || err.contains("error"), "{err}");
    }
}

#[test]
fn help_and_version_are_exit_0() {
    let env = Env::new();
    let (code, out, _) = ferry(&env, &["--help"]);
    assert_eq!(code, 0);
    for word in ["status", "release", "id", "trust", "layout", "--json"] {
        assert!(out.contains(word), "{word} missing from help:\n{out}");
    }
    let (code, out, _) = ferry(&env, &["layout", "place", "--help"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("--left-of") && out.contains("--align"),
        "{out}"
    );
    let (code, out, _) = ferry(&env, &["--version"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("ferry "), "{out}");
}

#[test]
fn ferryd_not_running_is_exit_3_with_a_friendly_message() {
    let env = Env::new();
    for args in [
        &["status"][..],
        &["id"],
        &["release"],
        &["layout", "show"],
        &["trust", &"ab".repeat(32)],
    ] {
        let (code, out, err) = ferry(&env, args);
        assert_eq!(code, 3, "{args:?}: {err}");
        assert_eq!(out, "");
        assert!(err.contains("ferryd is not running"), "{err}");
        assert!(err.contains("systemctl --user start ferryd"), "{err}");
    }
}

#[test]
fn a_stale_socket_is_also_exit_3() {
    let env = Env::new();
    std::fs::create_dir_all(env.paths().socket_dir()).unwrap();
    drop(std::os::unix::net::UnixListener::bind(env.socket()).unwrap());
    let (code, _, err) = ferry(&env, &["status"]);
    assert_eq!(code, 3);
    assert!(err.contains("stale"), "{err}");
}

#[test]
fn a_socket_that_is_not_ferryd_is_exit_1() {
    let env = Env::new();
    std::fs::create_dir_all(env.paths().socket_dir()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(env.socket()).unwrap();
    std::thread::spawn(move || {
        use std::io::Write;
        if let Ok((mut s, _)) = listener.accept() {
            let _ = s.write_all(b"hello there\n");
        }
    });
    let (code, _, err) = ferry(&env, &["status"]);
    assert_eq!(code, 1);
    assert!(err.contains("did not start with a ferryd hello"), "{err}");
}

#[test]
fn a_daemon_of_another_api_version_is_named() {
    let env = Env::new();
    std::fs::create_dir_all(env.paths().socket_dir()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(env.socket()).unwrap();
    std::thread::spawn(move || {
        use std::io::Write;
        if let Ok((mut s, _)) = listener.accept() {
            let _ = s.write_all(b"{\"event\":\"hello\",\"data\":{\"api\":7,\"version\":\"9.9.9\",\"peer\":\"x\",\"fingerprint\":\"\"}}\n");
        }
    });
    let (code, _, err) = ferry(&env, &["status"]);
    assert_eq!(code, 1);
    assert!(err.contains("API 7") && err.contains("9.9.9"), "{err}");
}

#[test]
fn release_reports_success_and_status_shows_the_forced_release() {
    let env = Env::new();
    let d = env.daemon("desk");
    let (code, out, _) = ferry(&env, &["release"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("Released"));
    assert_eq!(ferry(&env, &["release", "--json"]).1.trim(), "{}");
    let (_, out, _) = ferry(&env, &["status"]);
    assert!(out.contains("Last release: forced"), "{out}");
    d.shutdown();
}

#[test]
fn layout_show_lists_my_monitors_and_get_is_an_alias() {
    let env = Env::new();
    let d = env.daemon("desk");
    let (code, out, _) = ferry(&env, &["layout", "show"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("desk (this machine)  not placed  online"),
        "{out}"
    );
    assert!(out.contains("HDMI-A-1  1920x1080"), "{out}");
    assert_eq!(ferry(&env, &["layout", "get"]).1, out);
    let v: Value = serde_json::from_str(&ferry(&env, &["layout", "show", "--json"]).1).unwrap();
    assert_eq!(v["peers"][0]["monitors"][0]["name"], "HDMI-A-1");
    d.shutdown();
}

#[test]
fn layout_place_errors_come_back_as_exit_1() {
    let env = Env::new();
    let d = env.daemon("desk");
    let fp = "9c".repeat(32);
    ferry(&env, &["trust", &fp, "--name", "laptop"]);
    // laptop never connected, so its monitors are unknown (design §8 example)
    let (code, _, err) = ferry(
        &env,
        &[
            "layout", "place", "laptop", "--below", "HDMI-A-1", "--align", "center",
        ],
    );
    assert_eq!(code, 1);
    assert!(err.contains("laptop has never connected"), "{err}");
    assert!(err.contains("hint: start ferryd on that machine"), "{err}");
    d.shutdown();
}

/// A daemon whose `layout.toml` already knows `lap`'s one 1366x768 monitor, as if it had
/// connected once, and that pins `lap`.
fn desk_with_a_known_lap(env: &Env) -> (ferryd::Daemon, String) {
    let fp = "9c".repeat(32);
    let path = env.paths().layout_file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "[[known]]\npeer = \"{fp}\"\n[[known.monitor]]\nname = \"eDP-1\"\ndescription = \"\"\n\
             scale = 1.0\ntransform = 0\n[known.monitor.rect]\nx = 0\ny = 0\nw = 1366\nh = 768\n"
        ),
    )
    .unwrap();
    let d = env.daemon("desk");
    let out = env.ferry(&["trust", &fp, "--name", "lap"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    (d, fp)
}

/// `ferry layout set <peer> <x> <y>` puts a peer's group at an origin and prints the
/// layout as `layout place` does.
#[test]
fn layout_set_moves_a_peer_and_prints_the_layout() {
    let env = Env::new();
    let (d, fp) = desk_with_a_known_lap(&env);

    let (code, out, err) = ferry(&env, &["layout", "set", "lap", "0", "1080"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(
        out.starts_with("Layout revision 1 (set by desk)\n"),
        "{out}"
    );
    assert!(out.contains("lap  origin (0, 1080)  offline"), "{out}");
    assert!(
        out.contains("eDP-1  1366x768  at (0, 1080) on the plane"),
        "{out}"
    );
    assert!(
        out.contains("desk:HDMI-A-1") && out.contains("--bottom-->"),
        "{out}"
    );

    // a negative origin is a number, not a flag: lap to the left of desk
    let (code, out, err) = ferry(&env, &["layout", "set", "lap", "-1366", "0"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("lap  origin (-1366, 0)"), "{out}");

    // the fingerprint works as the name does, and --json is the daemon's answer, one line
    let (code, json, _) = ferry(&env, &["layout", "set", &fp[..8], "1920", "0", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(json.lines().count(), 1);
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["revision"]["counter"], 3);
    assert_eq!(v["peers"][1]["origin"], json!([1920, 0]));
    assert_eq!(v["peers"][1]["trusted"], true);
    assert_eq!(v["peers"][0]["self"], true);

    // the same origin again changes nothing: the revision stays
    let (_, again, _) = ferry(&env, &["layout", "set", "lap", "1920", "0", "--json"]);
    assert_eq!(serde_json::from_str::<Value>(&again).unwrap(), v);
    d.shutdown();
}

/// A placement that covers another peer is exit 1, naming it.
#[test]
fn layout_set_onto_another_peer_is_exit_1_and_names_it() {
    let env = Env::new();
    let (d, _) = desk_with_a_known_lap(&env);
    let (code, out, err) = ferry(&env, &["layout", "set", "lap", "100", "100"]);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(err.starts_with("error: lap overlaps desk\n"), "{err}");
    assert!(err.contains("  hint: "), "{err}");

    // under --json the error is the socket's: one line on stderr, the same exit code
    let (code, out, err) = ferry(&env, &["layout", "set", "lap", "100", "100", "--json"]);
    assert_eq!((code, out.as_str()), (1, ""));
    let v: Value = serde_json::from_str(&err).unwrap();
    assert_eq!(v["error"]["code"], "overlap");
    assert_eq!(v["error"]["message"], "lap overlaps desk");

    // nothing was saved
    let (_, shown, _) = ferry(&env, &["layout", "show", "--json"]);
    assert_eq!(
        serde_json::from_str::<Value>(&shown).unwrap()["revision"]["counter"],
        0
    );

    // a peer nobody knows
    let (code, _, err) = ferry(&env, &["layout", "set", "nobody", "0", "0"]);
    assert_eq!(code, 1);
    assert!(err.contains("no peer is named \"nobody\""), "{err}");
    d.shutdown();
}

#[test]
fn a_fingerprint_is_64_chars_in_every_output() {
    let env = Env::new();
    let d = env.daemon("desk");
    let v: Value = serde_json::from_str(&ferry(&env, &["id", "--json"]).1).unwrap();
    assert_eq!(v["fingerprint"].as_str().unwrap().len(), OWN_FP_LEN);
    d.shutdown();
}

/// FR-CLI-04 under `--json`: an error is one JSON line with a `code`, and the exit code stays.
#[test]
fn json_errors_carry_a_code() {
    let dir = tempfile::tempdir().unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = ferry_cli::run(
        ["ferry", "--json", "status"],
        &dir.path().join("ferryd.sock"),
        &mut out,
        &mut err,
    );
    assert_eq!(code, 3);
    assert!(out.is_empty());
    let v: serde_json::Value = serde_json::from_slice(&err).unwrap();
    assert_eq!(v["error"]["code"], "ferryd_unreachable");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not running")
    );
}

/// A writer for the command's output that fails with this kind of error.
struct Failing(std::io::ErrorKind);

impl std::io::Write for Failing {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(self.0.into())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(self.0.into())
    }
}

/// `ferry status | head -1` (the reader leaves before the output is written) is not a
/// ferryd error: nothing on stderr, exit 0, like `cat`. The real binary, a real daemon, and a
/// stdout whose reading end is already closed.
#[test]
fn a_closed_stdout_is_not_a_ferryd_error() {
    use std::os::fd::OwnedFd;
    use std::process::{Command, Stdio};

    let env = Env::new();
    let d = env.daemon("desk");
    let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
    drop(reader);
    let out = Command::new(env!("CARGO_BIN_EXE_ferry"))
        .arg("status")
        .env_clear()
        .env("HOME", &env.home)
        .env("XDG_RUNTIME_DIR", &env.run)
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .output()
        .unwrap();
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    d.shutdown();
}

/// C-04 in process, with `--json` too: a broken pipe ends the command quietly, and any other
/// failure to write the output says what it is, not "lost the connection to ferryd".
#[test]
fn a_write_error_is_named_as_the_output_and_a_broken_pipe_is_quiet() {
    let env = Env::new();
    let d = env.daemon("desk");
    for args in [&["status"][..], &["--json", "status"], &["id"], &["peers"]] {
        let mut err = Vec::new();
        let code = run(
            std::iter::once("ferry").chain(args.iter().copied()),
            &env.socket(),
            &mut Failing(std::io::ErrorKind::BrokenPipe),
            &mut err,
        );
        assert_eq!((code, text(&err).as_str()), (0, ""), "{args:?}");
    }
    let mut err = Vec::new();
    let code = run(
        ["ferry", "status"],
        &env.socket(),
        &mut Failing(std::io::ErrorKind::StorageFull),
        &mut err,
    );
    let err = text(&err);
    assert_eq!(code, 1, "{err}");
    assert!(err.starts_with("error: cannot write the output"), "{err}");
    assert!(!err.contains("ferryd"), "{err}");
    let mut err = Vec::new();
    run(
        ["ferry", "--json", "status"],
        &env.socket(),
        &mut Failing(std::io::ErrorKind::StorageFull),
        &mut err,
    );
    let v: Value = serde_json::from_slice(&err).unwrap();
    assert_eq!(v["error"]["code"], "output");
    d.shutdown();
}

/// The CLI has one default port, and it is the one ferryd binds.
#[test]
fn the_default_port_is_the_one_ferryd_binds() {
    assert_eq!(ferry_cli::DEFAULT_PORT, ferry_net::DEFAULT_PORT);
}

/// Without HOME, `setup` and `doctor` refuse with ferryd's own words, not `/.config`. The
/// commands that need no home directory go on working.
#[test]
fn without_a_home_setup_and_doctor_refuse_in_ferryds_words() {
    use std::process::Command;
    let env = Env::new();
    let no_home = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_ferry"))
            .args(args)
            .env_clear()
            .env("XDG_RUNTIME_DIR", &env.run)
            .output()
            .unwrap()
    };
    let want = format!("error: {}\n", ferryd::paths::PathsError::NoHome);
    for args in [
        &["setup", "--yes"][..],
        &["setup", "--undo", "--yes"],
        &["doctor"],
    ] {
        let out = no_home(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(text(&out.stderr), want, "{args:?}");
        assert_eq!(text(&out.stdout), "", "nothing was done: {args:?}");
    }
    let out = no_home(&["--json", "doctor"]);
    let v: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(v["error"]["code"], "no_home");
    // status needs a socket and no home: with no daemon it is exit 3, not the home error
    let out = no_home(&["status"]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    // a relative HOME is no home either, as for ferryd
    let out = Command::new(env!("CARGO_BIN_EXE_ferry"))
        .args(["setup", "--yes"])
        .env_clear()
        .env("HOME", "relative/home")
        .env("XDG_RUNTIME_DIR", &env.run)
        .output()
        .unwrap();
    assert_eq!(text(&out.stderr), want);
}

// --- ferry status: state, escape chord, attention ----------------------------

#[test]
fn status_shows_the_state_the_escape_chord_and_what_needs_attention() {
    let env = Env::new();
    let cfg = env.paths().config_file;
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "[peer]\nnmae = \"typo\"\n").unwrap();
    let d = env.daemon("desk");
    let (code, out, _) = ferry(&env, &["status"]);
    assert_eq!(code, 0);
    assert!(out.contains("  state        attention\n"), "{out}");
    assert!(out.contains("  escape       Super+Ctrl+Escape\n"), "{out}");
    assert!(
        out.contains("\nAttention\n  config.toml: unknown key peer.nmae\n    hint: "),
        "{out}"
    );
    let v: Value = serde_json::from_str(&ferry(&env, &["status", "--json"]).1).unwrap();
    assert_eq!(v["summary"]["state"], "attention");
    assert_eq!(v["attention"][0]["code"], "config_invalid");
    assert_eq!(v["escape_chord"], "Super+Ctrl+Escape");
    d.shutdown();
}

// --- ferry config -----------------------------------------------------------------

#[test]
fn config_set_checks_through_ferryd_and_get_reads_it_back() {
    let env = Env::new();
    let d = env.daemon("desk");

    let (code, out, err) = ferry(&env, &["config", "get", "crossing.edge_resistance_px"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "0\n", ""));

    let (code, out, err) = ferry(
        &env,
        &["config", "set", "crossing.edge_resistance_px", "40"],
    );
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(out, "crossing.edge_resistance_px = 40 (applied)\n");
    let (_, out, _) = ferry(&env, &["config", "get", "crossing.edge_resistance_px"]);
    assert_eq!(out, "40\n");
    // the file says it, as a person would have written it
    let file = std::fs::read_to_string(env.paths().config_file).unwrap();
    assert_eq!(file, "[crossing]\nedge_resistance_px = 40\n");

    // booleans read the usual words; a name that looks like a boolean is still a name
    let (code, out, _) = ferry(
        &env,
        &["config", "set", "crossing.block_while_button_held", "off"],
    );
    assert_eq!(
        (code, out.as_str()),
        (0, "crossing.block_while_button_held = false (applied)\n")
    );
    let (code, out, _) = ferry(&env, &["config", "set", "peer.name", "true"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("peer.name = true (saved; applies after systemctl --user restart ferryd)"),
        "{out}"
    );
    assert_eq!(ferry(&env, &["config", "get", "peer.name"]).1, "true\n");

    // the whole list marks what waits for a restart
    let (code, out, _) = ferry(&env, &["config", "get"]);
    assert_eq!(code, 0);
    let row = |key: &str| -> Vec<String> {
        out.lines()
            .find(|l| l.starts_with(key))
            .unwrap_or_else(|| panic!("{key} missing:\n{out}"))
            .split_whitespace()
            .map(str::to_string)
            .collect()
    };
    assert_eq!(
        row("crossing.edge_resistance_px"),
        ["crossing.edge_resistance_px", "40"]
    );
    assert!(
        out.contains("peer.name") && out.contains("(saved; applies after a restart of ferryd)"),
        "{out}"
    );

    // the escape chord
    let (code, out, _) = ferry(
        &env,
        &["config", "set", "crossing.escape_chord", "ctrl+alt+f12"],
    );
    assert_eq!(code, 0);
    assert!(
        out.contains("Ctrl+Alt+F12") && out.contains("ferry setup"),
        "{out}"
    );
    d.shutdown();
}

/// "Invalid value exits 1 with the allowed range."
#[test]
fn config_set_with_a_refused_value_exits_1_and_says_what_is_allowed() {
    let env = Env::new();
    let d = env.daemon("desk");
    for (args, message, hint) in [
        (
            &["crossing.edge_resistance_px", "900"],
            "out of range",
            "0 to 200 px",
        ),
        (
            &["crossing.edge_resistance_px", "lots"],
            "expected a whole number",
            "0 to 200 px",
        ),
        (
            &["safety.lock_check_budget_ms", "10"],
            "out of range",
            "50 to 100 ms",
        ),
        (
            &["input.capture", "telepathy"],
            "not one of the choices",
            "one of auto, portal",
        ),
        (
            &["crossing.block_while_button_held", "maybe"],
            "expected true or false",
            "true or false",
        ),
        (
            &["crossing.escape_chord", "Escape"],
            "2 modifiers",
            "at least two modifiers",
        ),
        (&["peer.name", "two words"], "peer.name", "no whitespace"),
        (&["peer.nope", "1"], "no config key", "ferry config schema"),
    ] {
        let mut argv = vec!["config", "set"];
        argv.extend(args);
        let (code, out, err) = ferry(&env, &argv);
        assert_eq!(code, 1, "{args:?}: {err}");
        assert_eq!(out, "");
        assert!(
            err.starts_with("error: ") && err.contains(message),
            "{args:?}: {err}"
        );
        assert!(
            err.contains(&format!("hint: {hint}")) || err.contains(hint),
            "{args:?}: {err}"
        );
    }
    assert!(
        !env.paths().config_file.exists(),
        "no refusal wrote the file"
    );

    // --json: one line on stderr, the shape of the socket API's errors
    let (code, _, err) = ferry(&env, &["--json", "config", "set", "peer.port", "70000"]);
    assert_eq!(code, 1);
    let v: Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(v["error"]["code"], "bad_value");
    assert_eq!(v["error"]["hint"], "0 to 65535");
    d.shutdown();
}

#[test]
fn config_schema_lists_every_key_and_json_is_the_daemons_answer() {
    let env = Env::new();
    let d = env.daemon("desk");
    let (code, out, err) = ferry(&env, &["config", "schema"]);
    assert_eq!((code, err.as_str()), (0, ""));
    for key in ferryd::config::KEYS {
        assert!(
            out.contains(&format!("{}\n", key.key)),
            "{} missing:\n{out}",
            key.key
        );
    }
    assert!(
        out.contains("a whole number, 0 to 200 px; default 0; applies at once"),
        "{out}"
    );
    assert!(
        out.contains("one of auto, portal, direct, layer-shell, off; default auto"),
        "{out}"
    );
    let v: Value = serde_json::from_str(&ferry(&env, &["config", "schema", "--json"]).1).unwrap();
    assert_eq!(
        v["keys"].as_array().unwrap().len(),
        ferryd::config::KEYS.len()
    );
    let v: Value = serde_json::from_str(&ferry(&env, &["config", "get", "--json"]).1).unwrap();
    assert_eq!(v["values"]["peer.port"], 4243);
    let v: Value = serde_json::from_str(
        &ferry(
            &env,
            &["--json", "config", "set", "crossing.edge_dwell_ms", "100"],
        )
        .1,
    )
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({"key": "crossing.edge_dwell_ms", "value": 100, "applies": "live"})
    );
    d.shutdown();
}

// --- ferry events -----------------------------------------------------------------------------

#[test]
fn events_prints_the_hello_and_then_the_raw_lines_of_the_topics_asked_for() {
    let env = Env::new();
    let d = env.daemon("desk");
    let (code, out, err) = ferry(
        &env,
        &["events", "--topic", "state", "-t", "peers", "--count", "2"],
    );
    assert_eq!((code, err.as_str()), (0, ""));
    let lines: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert_eq!(lines[0]["event"], "hello");
    assert!(lines[0]["data"]["methods"].as_array().unwrap().len() > 15);
    assert_eq!(lines[1]["event"], "state");
    assert_eq!(lines[1]["data"]["summary"]["state"], "idle");
    assert_eq!(lines[2]["event"], "peers");
    // -n 0 is the hello alone; --json changes nothing, the lines are JSON already
    let (code, out, _) = ferry(&env, &["--json", "events", "-n", "0"]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 1);
    d.shutdown();
}

#[test]
fn events_follows_a_change_and_ends_when_ferryd_goes() {
    let env = Env::new();
    let d = env.daemon("desk");
    // the real binary, so the lines are flushed through a pipe as a frontend author sees them
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_ferry"))
        .args(["events", "-t", "state"])
        .env_clear()
        .env("HOME", &env.home)
        .env("XDG_RUNTIME_DIR", &env.run)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut read = || {
        let mut l = String::new();
        std::io::BufRead::read_line(&mut lines, &mut l).unwrap();
        serde_json::from_str::<Value>(&l).unwrap_or_else(|e| panic!("{l:?}: {e}"))
    };
    assert_eq!(read()["event"], "hello");
    assert_eq!(read()["data"]["locked"], false);
    assert_eq!(ferry(&env, &["lock", "on"]).0, 0);
    assert_eq!(read()["data"]["locked"], true);
    d.shutdown();
    // ferryd stopped: the stream ends and the command says so
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("ferryd closed the connection"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn events_with_an_unknown_topic_is_a_usage_error_naming_the_topics() {
    let env = Env::new();
    let (code, out, err) = ferry(&env, &["events", "--topic", "fax"]);
    assert_eq!(code, 2);
    assert_eq!(out, "");
    assert!(err.contains("fax") && err.contains("notice"), "{err}");
}
