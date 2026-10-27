//! `ferry ui <view>`: the real `ferry` binary, with a script on its PATH
//! standing in for `omarchy-shell`, and no HOME. Nothing here touches a real shell.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

use std::path::PathBuf;
use std::process::{Command, Output};

mod support;
use serde_json::Value;
use support::{script, text};

/// A directory with a fake `omarchy-shell` in it. The script writes down its arguments and the
/// `OMARCHY_PATH` it got, says `$FAKE_SAY` on stderr and exits with `$FAKE_CODE`.
struct Rig {
    tmp: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::Builder::new().prefix("fu").tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("bin")).unwrap();
        std::fs::create_dir(tmp.path().join("empty")).unwrap();
        script(
            &tmp.path().join("bin/omarchy-shell"),
            "echo \"$*\" >> \"$FAKE_LOG\"\n\
             echo \"OMARCHY_PATH=$OMARCHY_PATH\" >> \"$FAKE_LOG\"\n\
             if [ -n \"$FAKE_SAY\" ]; then echo \"$FAKE_SAY\" >&2; fi\n\
             exit \"${FAKE_CODE:-0}\"",
        );
        Self { tmp }
    }

    fn log_path(&self) -> PathBuf {
        self.tmp.path().join("log")
    }

    /// `ferry <args>` with a PATH that holds the fake shell and nothing else, and no HOME.
    fn ferry(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.ferry_on(&self.tmp.path().join("bin"), args, env)
    }

    fn ferry_on(&self, path: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ferry"))
            .args(args)
            .env_clear()
            .env("PATH", path)
            .env("FAKE_LOG", self.log_path())
            .envs(env.iter().copied())
            .output()
            .unwrap()
    }

    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.log_path())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

#[test]
fn each_view_calls_its_function_of_the_ferry_target() {
    for (view, function) in [
        ("arrange", "arrange"),
        ("pair", "pair"),
        ("settings", "settings"),
        ("popover", "open"),
    ] {
        let rig = Rig::new();
        let out = rig.ferry(&["ui", view], &[]);
        assert_eq!(out.status.code(), Some(0), "{view}: {}", text(&out.stderr));
        assert!(
            out.stdout.is_empty() && out.stderr.is_empty(),
            "{view}: nothing is printed"
        );
        assert_eq!(
            rig.log(),
            [
                format!("ferry {function}"),
                "OMARCHY_PATH=/usr/share/omarchy".to_string()
            ],
            "{view}: the shell's IPC target is `ferry`, and a launcher entry has no OMARCHY_PATH"
        );
    }
}

#[test]
fn omarchy_path_is_passed_on_when_it_is_absolute() {
    for (given, passed) in [
        (Some("/home/me/omarchy"), "/home/me/omarchy"),
        (Some("omarchy"), "/usr/share/omarchy"),
        (Some(""), "/usr/share/omarchy"),
        (None, "/usr/share/omarchy"),
    ] {
        let rig = Rig::new();
        let env: Vec<(&str, &str)> = given.map(|g| ("OMARCHY_PATH", g)).into_iter().collect();
        let out = rig.ferry(&["ui", "pair"], &env);
        assert_eq!(out.status.code(), Some(0), "{given:?}");
        assert_eq!(rig.log()[1], format!("OMARCHY_PATH={passed}"), "{given:?}");
    }
}

#[test]
fn a_successful_call_prints_nothing_under_json_either() {
    let rig = Rig::new();
    let out = rig.ferry(&["--json", "ui", "settings"], &[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
}

fn error_json(out: &Output) -> Value {
    let lines: Vec<&str> = std::str::from_utf8(&out.stderr).unwrap().lines().collect();
    assert_eq!(lines.len(), 1, "one line: {lines:?}");
    let v: Value = serde_json::from_str(lines[0]).unwrap();
    v["error"].clone()
}

#[test]
fn a_machine_without_the_omarchy_shell_says_so() {
    let rig = Rig::new();
    let empty = rig.tmp.path().join("empty");
    let out = rig.ferry_on(&empty, &["ui", "arrange"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "error: omarchy-shell is not installed\n  hint: the ferry panels are a plugin for the Omarchy shell; other frontends read ferryd's socket instead\n"
    );
    assert!(out.stdout.is_empty());

    let out = rig.ferry_on(&empty, &["--json", "ui", "arrange"], &[]);
    assert_eq!(out.status.code(), Some(1));
    let e = error_json(&out);
    assert_eq!(e["code"], "omarchy_shell_missing");
    assert_eq!(e["message"], "omarchy-shell is not installed");
    assert!(
        e["hint"]
            .as_str()
            .unwrap()
            .contains("plugin for the Omarchy shell")
    );
}

#[test]
fn a_plugin_the_shell_does_not_have_points_at_setup() {
    let rig = Rig::new();
    let env = [("FAKE_SAY", "Target not found."), ("FAKE_CODE", "1")];
    let out = rig.ferry(&["ui", "arrange"], &env);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "error: the ferry plugin is not loaded in the shell (it answered: Target not found.)\n  hint: run ferry setup, which links and enables the ferry.bar plugin\n"
    );
    let out = rig.ferry(&["ui", "arrange", "--json"], &env);
    assert_eq!(out.status.code(), Some(1));
    let e = error_json(&out);
    assert_eq!(e["code"], "plugin_not_loaded");
    assert_eq!(
        e["hint"],
        "run ferry setup, which links and enables the ferry.bar plugin"
    );
    assert!(e["message"].as_str().unwrap().contains("Target not found."));
}

#[test]
fn a_shell_that_is_not_up_says_to_start_it() {
    for said in [
        "omarchy-shell is not running",
        "omarchy-shell is not responding",
        "omarchy-shell is not ready",
    ] {
        let rig = Rig::new();
        let env = [("FAKE_SAY", said), ("FAKE_CODE", "1")];
        let out = rig.ferry(&["ui", "pair"], &env);
        assert_eq!(out.status.code(), Some(1), "{said}");
        assert_eq!(
            text(&out.stderr),
            format!("error: {said}\n  hint: start the shell, then try again\n")
        );
        let e = error_json(&rig.ferry(&["--json", "ui", "pair"], &env));
        assert_eq!(e["code"], "shell_not_running", "{said}");
        assert_eq!(e["message"], said);
    }
}

#[test]
fn anything_else_the_shell_says_is_passed_on_as_it_is() {
    let rig = Rig::new();
    let env = [("FAKE_SAY", "Function not found."), ("FAKE_CODE", "1")];
    let out = rig.ferry(&["ui", "settings"], &env);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stderr), "error: Function not found.\n");
    let e = error_json(&rig.ferry(&["--json", "ui", "settings"], &env));
    assert_eq!(e["code"], "ui_failed");
    assert!(e.get("hint").is_none(), "no hint to give: {e}");

    // a failure with nothing to say still says it failed, and how
    let out = rig.ferry(&["ui", "settings"], &[("FAKE_CODE", "3")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with("error: omarchy-shell exited with "),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn a_view_that_is_not_one_is_a_usage_error_and_the_shell_is_not_asked() {
    let rig = Rig::new();
    let out = rig.ferry(&["ui", "wallpaper"], &[]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    for view in ["arrange", "pair", "settings", "popover"] {
        assert!(err.contains(view), "{err}");
    }
    assert!(rig.log().is_empty());
    assert_eq!(rig.ferry(&["ui"], &[]).status.code(), Some(2));
    assert_eq!(
        rig.ferry(&["ui", "arrange", "pair"], &[]).status.code(),
        Some(2)
    );
    assert!(rig.log().is_empty());
}

#[test]
fn ferry_ui_needs_no_ferryd() {
    // no XDG_RUNTIME_DIR, no socket: the shell is the only thing it talks to
    let rig = Rig::new();
    let out = rig.ferry(&["ui", "pair"], &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}
