//! `ferry doctor [--json]`: ferryd's checks (the `doctor` socket
//! method) merged with the ones only the CLI can make, because they do not need ferryd or must
//! work when it is down: the daemon itself, its unit, the xdph package, the key files, the
//! firewall. Exit 1 on any FAIL.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use ferry_core::api::Request;
use ferry_core::doctor::{Check, DoctorResult, Level, version_at_least};

use crate::client::{Client, ClientError};
use crate::setup::{Env, config_port, fsx};

/// The oldest xdph with the InputCapture portal ferry was tested against.
pub const MIN_XDPH: (u32, u32, u32) = (1, 4, 1);
const XDPH_PACKAGE: &str = "xdg-desktop-portal-hyprland";

/// How long a read-only probe (`systemctl --user show`, `pacman -Q`) may take. The doctor exists
/// for machines where things are broken: a hung user bus or a locked package database must not
/// hang it too.
const PROBE_WAIT: Duration = Duration::from_secs(5);

/// The order of the rows. A check not in the list (a `peer <name>` row) sorts by its prefix.
const ORDER: &[&str] = &[
    "daemon",
    "unit",
    "hyprland",
    "xdph",
    "capture",
    "globals",
    "barriers",
    "escape bind",
    "release_pressed_on_close",
    "key files",
    "plugin",
    "notifications",
    "port",
    "firewall",
    "lock check",
    "peers",
    "lock to this machine",
];

fn rank(name: &str) -> usize {
    let key = if name.starts_with("peer ") {
        "peers"
    } else {
        name
    };
    ORDER.iter().position(|n| *n == key).unwrap_or(ORDER.len())
}

/// Runs the doctor and prints it. Returns the exit code: 1 when any check failed.
pub fn run(socket: &Path, env: &Env, json: bool, out: &mut dyn Write) -> u8 {
    let report = collect(socket, env);
    let failures = report.count(Level::Fail);
    let warnings = report.count(Level::Warn);
    if json {
        let v = serde_json::json!({
            "checks": report.checks,
            "warnings": warnings,
            "failures": failures,
        });
        let _ = writeln!(out, "{v}");
    } else {
        let _ = write!(out, "{}", render(&report, warnings, failures));
    }
    u8::from(failures > 0)
}

/// ferryd's checks and ours, in table order.
pub fn collect(socket: &Path, env: &Env) -> DoctorResult {
    let mut checks = Vec::new();
    let mut remote = Vec::new();
    match Client::connect(socket) {
        Ok(mut c) => {
            let hello = c.hello.clone();
            checks.push(daemon_check(
                &hello.version,
                hello.api,
                env!("CARGO_PKG_VERSION"),
            ));
            match c.call(Request::Doctor) {
                Ok(v) => match serde_json::from_value::<DoctorResult>(v) {
                    Ok(r) => remote = r.checks,
                    Err(e) => checks.push(Check::fail(
                        "daemon",
                        format!("ferryd's doctor answer has an unexpected shape: {e}"),
                        "update ferryd and ferry to the same version",
                    )),
                },
                Err(e) => checks.push(Check::fail(
                    "daemon",
                    format!("ferryd could not run its checks: {e}"),
                    "journalctl --user -u ferryd",
                )),
            }
        }
        Err(e) => checks.push(daemon_down(&e)),
    }
    let daemon_up = !remote.is_empty();
    if !remote.iter().any(|c| c.name == "unit") {
        // ferryd names the unit only when it runs under one other than ferryd.service.
        checks.push(unit(env, daemon_up));
    }
    checks.push(xdph(env));
    checks.push(key_files(env));
    checks.push(crate::setup::plugin::doctor_check(env));
    checks.push(notifications(env));
    if !remote.iter().any(|c| c.name == "firewall") {
        // ferryd is down: ufw's side only.
        let conf = fsx::read_lossy(&env.ufw_conf).ok().flatten();
        checks.push(ferry_core::doctor::firewall_check(
            conf.as_deref(),
            config_port(env),
            None,
        ));
    }
    checks.extend(remote);
    checks.sort_by_key(|c| rank(&c.name));
    DoctorResult { checks }
}

/// The `daemon` row of a ferryd that answers. After a package upgrade the running ferryd is still
/// the old binary until it is restarted.
pub fn daemon_check(version: &str, api: u32, ours: &str) -> Check {
    if version == ours {
        Check::pass("daemon", format!("ferryd {version}, api {api}"))
    } else {
        Check::warn(
            "daemon",
            format!("ferryd {version}, api {api}, but this ferry is {ours}"),
            "systemctl --user restart ferryd",
        )
    }
}

fn daemon_down(e: &ClientError) -> Check {
    Check::fail(
        "daemon",
        match e {
            ClientError::NotRunning(_) | ClientError::Stale(_) => {
                "ferryd is not running".to_string()
            }
            other => other
                .to_string()
                .lines()
                .next()
                .unwrap_or_default()
                .to_string(),
        },
        "systemctl --user start ferryd   (or `ferry setup`)",
    )
}

/// The unit's state from `systemctl --user show`, against whether ferryd answers.
fn unit(env: &Env, daemon_up: bool) -> Check {
    let shown = probe(
        &env.systemctl,
        &[
            "--user",
            "show",
            "ferryd",
            "--property=UnitFileState,ActiveState,SubState,NRestarts,FragmentPath",
        ],
        PROBE_WAIT,
    );
    let shown = match shown {
        Ok(shown) => shown,
        Err(ProbeError::TimedOut) => {
            return Check::warn(
                "unit",
                format!("systemctl did not answer within {} s", PROBE_WAIT.as_secs()),
                "is the user session running? try: systemctl --user status",
            );
        }
        Err(ProbeError::Spawn) => {
            return Check::warn("unit", "systemctl is not available", "");
        }
    };
    let text = String::from_utf8_lossy(&shown.stdout);
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let check = unit_check(
        &get("UnitFileState"),
        &get("ActiveState"),
        &get("SubState"),
        get("NRestarts").parse().unwrap_or(0),
        daemon_up,
    );
    match shadow_check(&get("FragmentPath"), &env.packaged_unit) {
        Some(shadow) if check.level != Level::Fail => shadow,
        _ => check,
    }
}

/// Why a probe gave no output.
#[derive(Debug)]
enum ProbeError {
    Spawn,
    /// It ran past the limit and was killed.
    TimedOut,
}

/// Runs a read-only program and waits at most `limit` for it. On expiry the program and what it
/// started (its own process group) are killed. Nothing here is for `sudo`, which waits for the
/// person at the keyboard.
fn probe(program: &Path, args: &[&str], limit: Duration) -> Result<Output, ProbeError> {
    use std::os::unix::process::CommandExt;
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| ProbeError::Spawn)?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    // the reader owns the child, so a hung probe is left behind rather than waited for
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(limit) {
        Ok(output) => output.map_err(|_| ProbeError::Spawn),
        Err(_) => {
            if let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            Err(ProbeError::TimedOut)
        }
    }
}

/// A unit file in the user's config directory wins over the packaged one, so after installing the
/// package an older unit (written by `ferry setup` for a build without a package) keeps running
/// the old `ferryd`.
pub fn shadow_check(fragment: &str, packaged: &Path) -> Option<Check> {
    let same = |a: &Path, b: &Path| {
        a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
    };
    let fragment = Path::new(fragment);
    if fragment.as_os_str().is_empty() || !packaged.is_file() || same(fragment, packaged) {
        return None;
    }
    Some(Check::warn(
        "unit",
        format!(
            "{} shadows the packaged {}",
            fragment.display(),
            packaged.display()
        ),
        "ferry setup   (it removes a unit it wrote; delete one you wrote yourself)",
    ))
}

/// The rules of the `unit` row. `Restart=on-failure` with no start limit never leaves a crashing
/// ferryd in `failed`: it sits in `activating (auto-restart)`, and that is the failure to name.
pub fn unit_check(
    file_state: &str,
    active: &str,
    sub: &str,
    restarts: u32,
    daemon_up: bool,
) -> Check {
    if active == "failed" {
        return Check::fail(
            "unit",
            "ferryd.service failed",
            "journalctl --user -u ferryd",
        );
    }
    if sub == "auto-restart" {
        return Check::fail(
            "unit",
            format!(
                "ferryd.service keeps crashing and being restarted ({restarts} restarts so far)"
            ),
            "journalctl --user -u ferryd",
        );
    }
    match (file_state == "enabled", active == "active") {
        (true, true) => Check::pass("unit", "ferryd.service enabled, active"),
        (_, true) => Check::warn(
            "unit",
            "ferryd.service is active but not enabled: it will not start at login",
            "ferry setup",
        ),
        (_, false) if daemon_up => Check::warn(
            "unit",
            "ferryd runs outside systemd: no watchdog, no restart after a crash",
            "ferry setup   (then stop the ferryd you started by hand)",
        ),
        (true, false) => Check::warn(
            "unit",
            "ferryd.service is enabled but not active",
            "systemctl --user start ferryd",
        ),
        (false, false) => Check::warn("unit", "ferryd.service is not enabled", "ferry setup"),
    }
}

/// `pacman -Q xdg-desktop-portal-hyprland` against the minimum.
fn xdph(env: &Env) -> Check {
    let out = probe(&env.pacman, &["-Q", XDPH_PACKAGE], PROBE_WAIT);
    let text = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(ProbeError::TimedOut) => {
            return Check::warn(
                "xdph",
                format!("pacman did not answer within {} s", PROBE_WAIT.as_secs()),
                "is another pacman running? it may hold the package database",
            );
        }
        _ => {
            return Check::warn(
                "xdph",
                format!("{XDPH_PACKAGE} not found through pacman"),
                format!(
                    "pacman -S {XDPH_PACKAGE}   (skip this if another package manager installed it)"
                ),
            );
        }
    };
    let version = text.split_whitespace().nth(1).unwrap_or_default();
    match version_at_least(version, MIN_XDPH) {
        Some(true) => Check::pass("xdph", format!("{XDPH_PACKAGE} {version}")),
        Some(false) => Check::fail(
            "xdph",
            format!("{XDPH_PACKAGE} {version} is older than 1.4.1"),
            "update xdg-desktop-portal-hyprland",
        ),
        None => Check::warn("xdph", format!("cannot read a version from {text:?}"), ""),
    }
}

/// Whether a notification daemon owns `org.freedesktop.Notifications` on the session bus. Without
/// one, ferryd's notices (a peer refused, a lock, a pairing request) are never shown.
fn notifications(env: &Env) -> Check {
    const NAME: &str = "notifications";
    const BUS_NAME: &str = "org.freedesktop.Notifications";
    let out = probe(
        &env.busctl,
        &[
            "--user",
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            "s",
            BUS_NAME,
        ],
        PROBE_WAIT,
    );
    match out {
        Ok(o) if o.status.success() => match String::from_utf8_lossy(&o.stdout).trim() {
            "b true" => Check::pass(NAME, format!("{BUS_NAME} has an owner")),
            "b false" => Check::warn(
                NAME,
                format!("nothing owns {BUS_NAME}: ferry's notices are not shown"),
                "omarchy restart shell   (the shell is Omarchy's notification daemon)",
            ),
            other => Check::warn(NAME, format!("busctl answered {other:?}"), ""),
        },
        Err(ProbeError::TimedOut) => Check::warn(
            NAME,
            format!(
                "the session bus did not answer within {} s",
                PROBE_WAIT.as_secs()
            ),
            "is the user session running? try: systemctl --user status",
        ),
        _ => Check::warn(NAME, "cannot ask the session bus (busctl failed)", ""),
    }
}

/// The key and certificate are 0600 and their directory 0700.
fn key_files(env: &Env) -> Check {
    let dir = env.data_home.join("ferry");
    let mode = |p: &Path| std::fs::metadata(p).map(|m| m.permissions().mode() & 0o777);
    let Ok(dir_mode) = mode(&dir) else {
        return Check::pass(
            "key files",
            "none yet: ferryd creates them on its first start",
        );
    };
    let mut bad: Vec<String> = Vec::new();
    let mut fix: Vec<String> = Vec::new();
    if dir_mode & 0o077 != 0 {
        bad.push(format!("{} is {dir_mode:o}", dir.display()));
        fix.push(format!("chmod 700 {}", dir.display()));
    }
    for name in ["key.der", "cert.der"] {
        let p = dir.join(name);
        if let Ok(m) = mode(&p)
            && m & 0o077 != 0
        {
            bad.push(format!("{name} is {m:o}"));
            fix.push(format!("chmod 600 {}", p.display()));
        }
    }
    if bad.is_empty() {
        Check::pass("key files", "key and certificate 0600, directory 0700")
    } else {
        Check::fail(
            "key files",
            format!("too open: {}", bad.join(", ")),
            fix.join(" && "),
        )
    }
}

pub fn render(report: &DoctorResult, warnings: usize, failures: usize) -> String {
    let width = report
        .checks
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(0)
        + 2;
    let mut out = String::new();
    for c in &report.checks {
        out.push_str(&format!(
            "{}  {:<width$}{}\n",
            c.level.label(),
            c.name,
            c.detail
        ));
        if let Some(fix) = &c.fix {
            out.push_str(&format!("      {:<width$}fix: {fix}\n", ""));
        }
    }
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    out.push_str(&format!(
        "{}, {}\n",
        plural(warnings, "warning"),
        plural(failures, "failure")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_come_out_in_table_order_with_peers_last() {
        let mut names = [
            "peer b", "firewall", "daemon", "weird", "capture", "peer a", "unit",
        ];
        names.sort_by_key(|n| rank(n));
        assert_eq!(names[..4], ["daemon", "unit", "capture", "firewall"]);
        assert_eq!(names[4..], ["peer b", "peer a", "weird"]);
    }

    #[test]
    fn the_unit_rules() {
        use ferry_core::doctor::Level::{Fail, Pass, Warn};
        let c = |f, a, s, up| unit_check(f, a, s, 3, up);
        assert_eq!(c("enabled", "active", "running", true).level, Pass);
        assert_eq!(c("enabled", "failed", "failed", false).level, Fail);
        // Restart=on-failure: a crash loop is activating, never failed
        let loop_ = c("enabled", "activating", "auto-restart", false);
        assert_eq!(loop_.level, Fail);
        assert!(loop_.detail.contains("3 restarts"), "{loop_:?}");
        assert_eq!(c("disabled", "inactive", "dead", true).level, Warn);
        assert_eq!(c("disabled", "inactive", "dead", false).level, Warn);
        assert_eq!(c("enabled", "inactive", "dead", false).level, Warn);
        assert_eq!(c("disabled", "active", "running", true).level, Warn);
        // a normal start-up is activating too, and is not a failure
        assert_eq!(c("enabled", "activating", "start", false).level, Warn);
    }

    #[test]
    fn an_older_running_ferryd_is_a_warning_with_the_restart_line() {
        let same = daemon_check("0.1.0", 1, "0.1.0");
        assert_eq!(same.level, Level::Pass);
        assert_eq!(same.detail, "ferryd 0.1.0, api 1");
        let old = daemon_check("0.1.0", 1, "0.2.0");
        assert_eq!(old.level, Level::Warn);
        assert!(old.detail.contains("this ferry is 0.2.0"), "{old:?}");
        assert_eq!(old.fix.as_deref(), Some("systemctl --user restart ferryd"));
    }

    #[test]
    fn a_user_unit_shadows_the_packaged_one() {
        let d = tempfile::tempdir().unwrap();
        let packaged = d.path().join("ferryd.service");
        let user = d.path().join("user.service");
        let none = d.path().join("none.service");
        // no package installed: nothing to shadow, whatever systemd reports
        assert!(shadow_check(user.to_str().unwrap(), &none).is_none());
        std::fs::write(&packaged, "").unwrap();
        assert!(shadow_check("", &packaged).is_none());
        assert!(shadow_check(packaged.to_str().unwrap(), &packaged).is_none());
        let w = shadow_check(user.to_str().unwrap(), &packaged).unwrap();
        assert_eq!(w.level, Level::Warn);
        assert!(w.detail.contains("shadows the packaged"), "{w:?}");
        // the same file reached through a symlink is not a shadow
        let link = d.path().join("link.service");
        std::os::unix::fs::symlink(&packaged, &link).unwrap();
        assert!(shadow_check(link.to_str().unwrap(), &packaged).is_none());
    }

    #[test]
    fn the_rendering_aligns_names_and_indents_the_fix() {
        let r = DoctorResult {
            checks: vec![
                Check::pass("daemon", "ferryd 0.1.0, api 1"),
                Check::warn("firewall", "ufw is on", "sudo ufw allow 4243/udp"),
            ],
        };
        let text = render(&r, 1, 0);
        let want = "PASS  daemon    ferryd 0.1.0, api 1\n\
                    WARN  firewall  ufw is on\n\
                    \x20     \x20         fix: sudo ufw allow 4243/udp\n\
                    1 warning, 0 failures\n";
        assert_eq!(text, want);
    }
}
