//! `ferry doctor`: the CLI's own checks, ferryd's merged in, the order,
//! the exit code and `--json`. ferryd is a fake on a socket in a temp dir that answers with
//! canned checks; the real ones are tested in ferryd (`doctor::tests`, `tests/api.rs`).
//! `systemctl` and `pacman` are scripts.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::time::Duration;

mod support;

use ferry_cli::setup::Env;
use serde_json::{Value, json};
use support::script;

struct Sandbox {
    tmp: tempfile::TempDir,
    env: Env,
    socket: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = tempfile::Builder::new().prefix("fd").tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        script(
            &bin.join("systemctl"),
            &unit_state("enabled", "active", "running"),
        );
        script(
            &bin.join("pacman"),
            "echo 'xdg-desktop-portal-hyprland 1.4.1-2'",
        );
        script(&bin.join("busctl"), "echo 'b true'");
        // the packaged plugin, linked and on the bar, as `ferry setup` leaves it
        let plugin = root.join("plugin");
        std::fs::create_dir_all(&plugin).unwrap();
        let omarchy = home.join(".config/omarchy");
        std::fs::create_dir_all(omarchy.join("plugins")).unwrap();
        std::os::unix::fs::symlink(&plugin, omarchy.join("plugins/ferry.bar")).unwrap();
        std::fs::write(
            omarchy.join("shell.json"),
            r#"{"version": 1, "bar": {"layout": {"right": [{"id": "ferry.bar"}]}}}"#,
        )
        .unwrap();
        let ufw = root.join("ufw.conf");
        std::fs::write(&ufw, "ENABLED=no\n").unwrap();
        let env = Env {
            home: home.clone(),
            config_home: home.join(".config"),
            data_home: home.join(".local/share"),
            runtime_dir: None,
            hypr_signature: None,
            systemctl: bin.join("systemctl"),
            sudo: bin.join("sudo"),
            pacman: bin.join("pacman"),
            busctl: bin.join("busctl"),
            ufw_conf: ufw,
            ufw_profiles: vec![],
            packaged_unit: root.join("none"),
            plugin_source: plugin,
            omarchy_path: root.join("none-omarchy"),
            omarchy_shell: bin.join("omarchy-shell"),
            ferryd: bin.join("ferryd"),
            socket: root.join("ferryd.sock"),
            interactive: false,
            hypr_wait: Duration::from_millis(100),
            unix_time: 0,
        };
        let socket = root.join("ferryd.sock");
        Self { tmp, env, socket }
    }

    /// A ferryd that says hello and answers `doctor` with `checks`.
    fn fake_ferryd(&self, checks: Value) {
        let listener = UnixListener::bind(&self.socket).unwrap();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let checks = checks.clone();
                std::thread::spawn(move || {
                    let mut w = conn.try_clone().unwrap();
                    let hello = json!({"event":"hello","data":{"api":1,"version":"0.1.0","peer":"desk","fingerprint":"00"}});
                    writeln!(w, "{hello}").unwrap();
                    for line in BufReader::new(conn).lines() {
                        let Ok(line) = line else { return };
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let result = match req["method"].as_str() {
                            Some("doctor") => json!({ "checks": checks }),
                            _ => json!({}),
                        };
                        writeln!(w, "{}", json!({"id": req["id"], "result": result})).unwrap();
                    }
                });
            }
        });
    }

    fn doctor(&self, json_out: bool) -> (u8, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let args: Vec<&str> = if json_out {
            vec!["ferry", "doctor", "--json"]
        } else {
            vec!["ferry", "doctor"]
        };
        let code = ferry_cli::run_in(
            args,
            &self.socket,
            &self.env,
            &mut std::io::empty(),
            &mut out,
            &mut err,
        );
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        (code, String::from_utf8(out).unwrap())
    }

    fn data_dir(&self) -> PathBuf {
        self.env.data_home.join("ferry")
    }
}

/// A `systemctl --user show` that answers with these states.
fn unit_state(file: &str, active: &str, sub: &str) -> String {
    format!(
        "shift\ncase \"$1\" in show) echo UnitFileState={file}; echo ActiveState={active}; echo SubState={sub}; echo NRestarts=2;; esac"
    )
}

fn pass(name: &str) -> Value {
    json!({"name": name, "level": "pass", "detail": format!("{name} ok")})
}

#[test]
fn ferryd_down_still_runs_the_cli_checks_and_fails() {
    let sb = Sandbox::new();
    let (code, out) = sb.doctor(false);
    assert_eq!(code, 1, "{out}");
    insta::assert_snapshot!("ferryd_down", sb.tmp_normalised(&out));
    assert!(out.contains("FAIL  daemon"), "{out}");
    assert!(out.contains("systemctl --user start ferryd"), "{out}");
    assert!(out.contains("PASS  xdph"), "{out}");
}

#[test]
fn a_healthy_machine_exits_0_with_the_rows_in_table_order() {
    let sb = Sandbox::new();
    sb.fake_ferryd(json!([
        pass("lock to this machine"),
        pass("peer lap"),
        pass("lock check"),
        pass("port"),
        pass("firewall"),
        pass("barriers"),
        pass("escape bind"),
        pass("release_pressed_on_close"),
        pass("globals"),
        pass("capture"),
        pass("hyprland"),
    ]));
    let (code, out) = sb.doctor(false);
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("healthy", out);
    assert!(out.ends_with("0 warnings, 0 failures\n"), "{out}");
}

#[test]
fn a_warning_does_not_fail_but_a_failure_does() {
    let sb = Sandbox::new();
    sb.fake_ferryd(json!([
        {"name": "firewall", "level": "warn", "detail": "ufw is on", "fix": "sudo ufw allow 4243/udp"},
        pass("hyprland"),
    ]));
    let (code, out) = sb.doctor(false);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("fix: sudo ufw allow 4243/udp"), "{out}");
    assert!(out.ends_with("1 warning, 0 failures\n"), "{out}");

    let sb = Sandbox::new();
    sb.fake_ferryd(json!([
        {"name": "capture", "level": "fail", "detail": "input capture not permitted", "fix": "add the permission line"},
    ]));
    let (code, out) = sb.doctor(false);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("FAIL  capture"), "{out}");
    assert!(out.ends_with("0 warnings, 1 failure\n"), "{out}");
}

#[test]
fn json_carries_the_checks_and_the_counts() {
    let sb = Sandbox::new();
    sb.fake_ferryd(json!([pass("hyprland")]));
    let (code, out) = sb.doctor(true);
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["failures"], 0);
    assert_eq!(v["warnings"], 0);
    let names: Vec<&str> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "daemon",
            "unit",
            "hyprland",
            "xdph",
            "key files",
            "plugin",
            "notifications",
            "firewall"
        ]
    );
    assert_eq!(v["checks"][0]["level"], "pass");
}

#[test]
fn ferrydown_json_is_still_one_line_of_json_with_exit_1() {
    let sb = Sandbox::new();
    let (code, out) = sb.doctor(true);
    assert_eq!(code, 1);
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert!(v["failures"].as_u64().unwrap() >= 1);
    assert_eq!(v["checks"][0]["name"], "daemon");
    assert_eq!(v["checks"][0]["level"], "fail");
}

#[test]
fn key_files_that_are_group_readable_fail_with_the_chmod() {
    let sb = Sandbox::new();
    let dir = sb.data_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    for f in ["key.der", "cert.der"] {
        std::fs::write(dir.join(f), b"x").unwrap();
        std::fs::set_permissions(dir.join(f), std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let (code, out) = sb.doctor(false);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("FAIL  key files"), "{out}");
    assert!(out.contains("chmod 700"), "{out}");
    assert!(out.contains("chmod 600"), "{out}");
    // tightened: pass
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    for f in ["key.der", "cert.der"] {
        std::fs::set_permissions(dir.join(f), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let (_, out) = sb.doctor(false);
    assert!(out.contains("PASS  key files"), "{out}");
}

#[test]
fn the_xdph_package_is_checked_against_1_4_1() {
    let sb = Sandbox::new();
    let pacman = &sb.env.pacman;
    script(pacman, "echo 'xdg-desktop-portal-hyprland 1.3.9-1'");
    let (code, out) = sb.doctor(false);
    assert_eq!(code, 1);
    assert!(out.contains("FAIL  xdph"), "{out}");
    script(pacman, "exit 1");
    let (_, out) = sb.doctor(false);
    assert!(out.contains("WARN  xdph"), "{out}");
    assert!(out.contains("not found through pacman"), "{out}");
}

#[test]
fn the_unit_row_follows_systemctl_and_whether_ferryd_answers() {
    let sb = Sandbox::new();
    let ctl = &sb.env.systemctl;
    // failed
    script(ctl, &unit_state("enabled", "failed", "failed"));
    let (code, out) = sb.doctor(false);
    assert_eq!(code, 1);
    assert!(out.contains("FAIL  unit"), "{out}");
    assert!(out.contains("journalctl --user -u ferryd"), "{out}");
    // not enabled, ferryd running by hand
    script(ctl, &unit_state("disabled", "inactive", "dead"));
    sb.fake_ferryd(json!([pass("hyprland")]));
    let (_, out) = sb.doctor(false);
    assert!(out.contains("WARN  unit"), "{out}");
    assert!(out.contains("ferryd runs outside systemd"), "{out}");
    // Restart=on-failure never leaves `failed`: a crash loop is activating, and is named
    script(ctl, &unit_state("enabled", "activating", "auto-restart"));
    let (code, out) = sb.doctor(false);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("FAIL  unit"), "{out}");
    assert!(out.contains("keeps crashing"), "{out}");
}

#[test]
fn the_firewall_row_comes_from_ufw_when_ferryd_is_down() {
    let sb = Sandbox::new();
    std::fs::write(&sb.env.ufw_conf, "ENABLED=yes\n").unwrap();
    let (_, out) = sb.doctor(false);
    assert!(out.contains("WARN  firewall"), "{out}");
    assert!(out.contains("sudo ufw allow 4243/udp"), "{out}");
}

/// With ferryd down, `doctor` names the port `config.toml` sets, as `setup` does, not 4243.
#[test]
fn doctor_with_ferryd_down_names_the_configured_port() {
    let sb = Sandbox::new();
    std::fs::write(&sb.env.ufw_conf, "ENABLED=yes\n").unwrap();
    let (_, out) = sb.doctor(false);
    assert!(out.contains("fix: sudo ufw allow 4243/udp"), "{out}");

    let cfg = sb.env.config_home.join("ferry");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("config.toml"), "[peer]\nport = 5000\n").unwrap();
    let (_, out) = sb.doctor(false);
    assert!(out.contains("fix: sudo ufw allow 5000/udp"), "{out}");
    assert!(!out.contains("4243"), "{out}");
    // the same reader as `setup`
    assert_eq!(ferry_cli::setup::config_port(&sb.env), 5000);
}

/// `systemctl --user show` on a hung user bus is a WARN after 5 s, not a hung doctor. The
/// script is `exec`ed so that killing it ends the sleep too.
#[test]
fn a_hung_systemctl_is_a_warning_and_not_a_hung_doctor() {
    let sb = Sandbox::new();
    script(&sb.env.systemctl, "exec sleep 60");
    let started = std::time::Instant::now();
    let (code, out) = sb.doctor(false);
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(code, 1, "ferryd is down: {out}");
    assert!(out.contains("WARN  unit"), "{out}");
    assert!(out.contains("systemctl did not answer within 5 s"), "{out}");
    assert!(
        out.contains("PASS  xdph"),
        "the other rows still come: {out}"
    );
}

/// The same for a `pacman` that waits on a locked database.
#[test]
fn a_hung_pacman_is_a_warning_and_not_a_hung_doctor() {
    let sb = Sandbox::new();
    script(&sb.env.pacman, "exec sleep 60");
    let started = std::time::Instant::now();
    let (_, out) = sb.doctor(false);
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "{:?}",
        started.elapsed()
    );
    assert!(out.contains("WARN  xdph"), "{out}");
    assert!(out.contains("pacman did not answer within 5 s"), "{out}");
    assert!(
        out.contains("PASS  unit"),
        "the other rows still come: {out}"
    );
}

impl Sandbox {
    fn tmp_normalised(&self, text: &str) -> String {
        text.replace(self.tmp.path().to_str().unwrap(), "<tmp>")
    }
}

#[test]
fn the_plugin_row_says_whether_the_widget_is_linked_and_on_the_bar() {
    let sb = Sandbox::new();
    let (_, out) = sb.doctor(false);
    assert!(out.contains("PASS  plugin"), "{out}");
    assert!(out.contains("linked, on the bar (right)"), "{out}");

    let shell = sb.env.home.join(".config/omarchy/shell.json");
    std::fs::write(
        &shell,
        r#"{"version": 1, "bar": {"layout": {"right": []}}}"#,
    )
    .unwrap();
    let (code, out) = sb.doctor(false);
    assert_eq!(code, 1, "ferryd is down: {out}");
    assert!(out.contains("WARN  plugin"), "{out}");
    assert!(out.contains("not on the bar"), "{out}");
    assert!(out.contains("omarchy plugin enable ferry.bar"), "{out}");

    std::fs::remove_file(sb.env.home.join(".config/omarchy/plugins/ferry.bar")).unwrap();
    let (_, out) = sb.doctor(false);
    assert!(out.contains("is not linked"), "{out}");
}

#[test]
fn the_notifications_row_asks_the_session_bus_for_an_owner() {
    let sb = Sandbox::new();
    let (_, out) = sb.doctor(false);
    assert!(out.contains("PASS  notifications"), "{out}");

    script(&sb.env.busctl, "echo 'b false'");
    let (_, out) = sb.doctor(false);
    assert!(out.contains("WARN  notifications"), "{out}");
    assert!(
        out.contains("nothing owns org.freedesktop.Notifications"),
        "{out}"
    );

    script(&sb.env.busctl, "exit 1");
    let (_, out) = sb.doctor(false);
    assert!(out.contains("WARN  notifications"), "{out}");
}
