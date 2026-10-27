//! The `doctor` socket method: the checks only ferryd can make.
//! `ferry doctor` merges them with its own (unit, xdph package, key files, ufw).
//!
//! [`collect`] reads the live facts: Hyprland over its IPC socket (read-only), the Wayland
//! globals (one registry round trip on a connection of its own, no device created), ferryd's own
//! status. [`build`] turns them into checks and touches nothing, so every rule has a unit test.

use std::time::Duration;

use ferry_core::api::{LinkState, StatusResult};
use ferry_core::doctor::{Check, DoctorResult, version_at_least};

use crate::ctl::Ctl;
use crate::live::{PortalHealth, Service};

/// The oldest Hyprland ferry is built and tested against.
pub const MIN_HYPRLAND: (u32, u32, u32) = (0, 56, 2);
/// How long a re-zone may take before the doctor calls it stuck (a normal one is ~100 ms).
pub const REZONE_STUCK: Duration = Duration::from_secs(5);
/// A peer whose latest round trip is slower than this is a WARN (NFR-PERF).
pub const RTT_WARN_US: u32 = 5_000;
/// The packaged ufw profile or a plain rule opens this port.
const UFW_CONF: &str = "/etc/ufw/ufw.conf";

/// What Hyprland says, read once.
#[derive(Debug, Clone)]
pub struct HyprFacts {
    pub version: Result<String, String>,
    /// A bind with `allow_input_capture` exists.
    pub capture_bind: Result<bool, String>,
    pub release_pressed_on_close: Result<bool, String>,
    pub enforce_permissions: Result<bool, String>,
}

impl HyprFacts {
    /// Nothing could be read.
    pub fn unavailable(why: &str) -> Self {
        Self {
            version: Err(why.to_string()),
            capture_bind: Err(why.to_string()),
            release_pressed_on_close: Err(why.to_string()),
            enforce_permissions: Err(why.to_string()),
        }
    }

    pub fn read(inst: Result<ferry_hypr_ipc::Instance, String>) -> Self {
        let inst = match inst {
            Ok(i) => i,
            Err(e) => return Self::unavailable(&format!("no Hyprland IPC: {e}")),
        };
        let version = inst
            .request("j/version")
            .map_err(|e| e.to_string())
            .and_then(|t| {
                serde_json::from_str::<serde_json::Value>(&t)
                    .map_err(|e| e.to_string())
                    .and_then(|v| {
                        v["version"]
                            .as_str()
                            .map(str::to_string)
                            .ok_or_else(|| "no version in the answer".to_string())
                    })
            });
        let option = |name: &str| -> Result<bool, String> {
            let t = inst
                .request(&format!("j/getoption {name}"))
                .map_err(|e| e.to_string())?;
            let v: serde_json::Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
            v["bool"]
                .as_bool()
                .or_else(|| v["int"].as_i64().map(|i| i != 0))
                .ok_or_else(|| format!("no value for {name}"))
        };
        let capture_bind = inst
            .request("j/binds")
            .map_err(|e| e.to_string())
            .and_then(|t| {
                let v: serde_json::Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
                let binds = v.as_array().ok_or("binds: not a list")?;
                Ok(binds
                    .iter()
                    .any(|b| b["allow_input_capture"].as_bool() == Some(true)))
            });
        Self {
            version,
            capture_bind,
            release_pressed_on_close: option("input:virtualkeyboard:release_pressed_on_close"),
            enforce_permissions: option("ecosystem:enforce_permissions"),
        }
    }
}

/// Everything [`build`] needs.
pub struct Inputs {
    pub status: StatusResult,
    /// This daemon captures (`input.capture != off` and input enabled).
    pub capture_on: bool,
    /// `input.capture = "direct"`: the backend that cannot work without the escape bind.
    pub direct: bool,
    pub portal: PortalHealth,
    pub rezoning_for: Option<Duration>,
    pub hypr: HyprFacts,
    /// Interface names of the Wayland globals, or why they could not be listed.
    pub globals: Result<Vec<String>, String>,
    /// `/etc/ufw/ufw.conf`, when readable.
    pub ufw_conf: Option<String>,
    /// The systemd service ferryd runs in, if any.
    pub service: Option<Service>,
    /// `[safety] lock_check_budget_ms`, in effect.
    pub lock_budget_ms: u32,
}

pub fn build(i: &Inputs) -> Vec<Check> {
    let mut out = vec![
        hyprland(i),
        capture(i),
        globals(i),
        barriers(i),
        escape_bind(i),
        release_pressed(i),
        port(i),
        firewall(i),
        lock_check(i),
    ];
    out.extend(peers(i));
    out.push(lock_here(i));
    out.extend(unit(i));
    out
}

/// The `unit` row, only when ferryd runs in a service other than `ferryd.service`: the CLI cannot
/// see that unit, and without this row it would call ferryd unmanaged. Under `ferryd.service`, or
/// outside any service, the CLI's own row (`systemctl --user show ferryd`) is the right one.
fn unit(i: &Inputs) -> Option<Check> {
    let s = i.service.as_ref().filter(|s| s.name != "ferryd.service")?;
    let watchdog = match s.watchdog {
        Some(d) if d.subsec_millis() == 0 => format!("systemd watchdog {} s", d.as_secs()),
        Some(d) => format!("systemd watchdog {} ms", d.as_millis()),
        None => "no systemd watchdog (WatchdogSec unset)".to_string(),
    };
    Some(Check::warn(
        "unit",
        format!(
            "ferryd runs under {}, not ferryd.service: {watchdog}; restart and start at login are that unit's",
            s.name
        ),
        "ferry setup   (installs ferryd.service; skip this for a test unit)",
    ))
}

fn hyprland(i: &Inputs) -> Check {
    match &i.hypr.version {
        Err(e) => Check::fail(
            "hyprland",
            e.clone(),
            "start ferryd inside the Hyprland session (systemctl --user restart ferryd)",
        ),
        Ok(v) => match version_at_least(v, MIN_HYPRLAND) {
            Some(true) => Check::pass("hyprland", format!("Hyprland {v}")),
            Some(false) => Check::fail(
                "hyprland",
                format!("Hyprland {v} is older than 0.56.2"),
                "update Hyprland",
            ),
            None => Check::warn(
                "hyprland",
                format!("cannot read Hyprland's version from {v:?}"),
                "",
            ),
        },
    }
}

/// Words a denied portal call is reported with.
pub fn looks_denied(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    [
        "permission",
        "denied",
        "not allowed",
        "not permitted",
        "cancelled",
    ]
    .iter()
    .any(|w| e.contains(w))
}

pub const PERMISSION_LINE: &str = r#"hl.permission({ binary = "/usr/lib/xdg-desktop-portal-hyprland", type = "input-capture", mode = "allow" })"#;

/// Hyprland adds `hl.permission` rules only on its first config parse; a reload skips them
/// without an error. So the line works only after a restart of Hyprland.
pub const PERMISSION_RESTART: &str = "restart Hyprland, which reads the rule only when it starts";

/// The capture row's fix: the line, and the restart it needs.
pub fn permission_fix() -> String {
    format!(
        "add to ~/.config/hypr/ferry.lua (or run `ferry setup`), then log out and back in (Hyprland reads hl.permission only when it starts): {PERMISSION_LINE}"
    )
}

fn capture(i: &Inputs) -> Check {
    if !i.capture_on {
        return Check::pass(
            "capture",
            "capture is off on this peer (input.capture = \"off\")",
        );
    }
    let not_permitted = |why: &str| {
        Check::fail(
            "capture",
            format!("input capture not permitted: {why}"),
            format!(
                "{}; if capture is still refused, the rule is not enough",
                permission_fix()
            ),
        )
    };
    if !i.portal.open {
        if let Some(e) = &i.portal.last_error
            && looks_denied(e)
        {
            return not_permitted(e);
        }
        return match &i.portal.last_error {
            Some(e) => Check::warn(
                "capture",
                format!(
                    "no portal session; retrying (attempt {}): {e}",
                    i.portal.failures
                ),
                "systemctl --user status xdg-desktop-portal-hyprland",
            ),
            None => Check::warn("capture", "the portal session is not open yet", ""),
        };
    }
    if i.hypr.enforce_permissions == Ok(true) {
        let crossed = i.status.counters.get("crossings").copied().unwrap_or(0) > 0;
        if !crossed {
            return Check::warn(
                "capture",
                format!(
                    "ecosystem:enforce_permissions is on and no crossing has started yet: answer Hyprland's permission dialog at the first crossing, or allow xdph in ferry.lua and {PERMISSION_RESTART}"
                ),
                permission_fix(),
            );
        }
    }
    Check::pass("capture", "a portal session is open")
}

fn globals(i: &Inputs) -> Check {
    match &i.globals {
        Err(e) => Check::fail(
            "globals",
            format!("cannot list the compositor's Wayland globals: {e}"),
            "is ferryd running inside the Hyprland session?",
        ),
        Ok(list) => {
            let missing: Vec<&str> = ferry_inject::REQUIRED_GLOBALS
                .iter()
                .copied()
                .filter(|g| !list.iter().any(|l| l == g))
                .collect();
            if missing.is_empty() {
                Check::pass("globals", "virtual pointer and keyboard managers present")
            } else {
                Check::fail(
                    "globals",
                    format!("the compositor does not offer {}", missing.join(", ")),
                    "this machine cannot be a target",
                )
            }
        }
    }
}

fn barriers(i: &Inputs) -> Check {
    if !i.capture_on {
        return Check::pass("barriers", "capture is off on this peer");
    }
    let c = &i.status.capture;
    if c.gate == "barriers_failed" || !c.failed_barriers.is_empty() {
        return Check::fail(
            "barriers",
            format!(
                "{} of {} barriers failed to set (barriers_failed)",
                c.failed_barriers.len().max(1),
                c.barriers
            ),
            "`ferry release`, then check the monitors with `hyprctl monitors`",
        );
    }
    if c.gate == "rezoning"
        && let Some(d) = i.rezoning_for
        && d > REZONE_STUCK
    {
        return Check::warn(
            "barriers",
            format!("re-zoning for {} s", d.as_secs()),
            "`ferry release`, then check the monitors",
        );
    }
    Check::pass(
        "barriers",
        format!("gate {}, {} barriers", c.gate, c.barriers),
    )
}

fn escape_bind(i: &Inputs) -> Check {
    if !i.capture_on {
        return Check::pass(
            "escape bind",
            "capture is off on this peer, so none is needed",
        );
    }
    match &i.hypr.capture_bind {
        Ok(true) => Check::pass("escape bind", "a bind with allow_input_capture is present"),
        Ok(false) if i.direct => Check::fail(
            "escape bind",
            "no escape bind, and the direct backend cannot be enabled without one",
            "ferry setup",
        ),
        Ok(false) => Check::warn(
            "escape bind",
            "no escape bind: if ferryd hangs, only the watchdog (up to 4 s) returns input",
            "ferry setup",
        ),
        Err(e) => Check::warn(
            "escape bind",
            format!("cannot read Hyprland's binds: {e}"),
            "",
        ),
    }
}

fn release_pressed(i: &Inputs) -> Check {
    match &i.hypr.release_pressed_on_close {
        Ok(true) => Check::pass("release_pressed_on_close", "on"),
        Ok(false) => Check::warn(
            "release_pressed_on_close",
            "off: a key held when a virtual keyboard dies is not released for the clients",
            "ferry setup",
        ),
        Err(e) => Check::warn(
            "release_pressed_on_close",
            format!("cannot read it: {e}"),
            "",
        ),
    }
}

fn port(i: &Inputs) -> Check {
    // ferryd would not be answering if the bind had failed.
    Check::pass("port", format!("UDP {} is bound", i.status.port))
}

fn firewall(i: &Inputs) -> Check {
    // The counter is absent until the first inbound connection.
    let accepted = i
        .status
        .counters
        .get("inbound_accepted")
        .copied()
        .unwrap_or(0);
    ferry_core::doctor::firewall_check(i.ufw_conf.as_deref(), i.status.port, Some(accepted))
}

fn lock_check(i: &Inputs) -> Check {
    if !i.capture_on {
        return Check::pass("lock check", "capture is off on this peer");
    }
    let n = i.status.counters.get("lock_unknown").copied().unwrap_or(0);
    let budget = i.lock_budget_ms;
    if n == 0 {
        Check::pass(
            "lock check",
            format!("every lock read was conclusive within the {budget} ms budget"),
        )
    } else {
        Check::warn(
            "lock check",
            format!(
                "{n} lock reads were inconclusive within the {budget} ms budget (treated as locked: a crossing waits)"
            ),
            "on a slow machine, raise [safety] lock_check_budget_ms (at most 100)",
        )
    }
}

/// Lock to this machine: a WARN when it is on, as information, so that
/// "why won't it cross" has an answer. The fix is the way out.
fn lock_here(i: &Inputs) -> Check {
    if i.status.locked {
        Check::warn(
            "lock to this machine",
            "on: this machine keeps the pointer, so no crossing starts from it",
            "ferry lock off",
        )
    } else {
        Check::pass("lock to this machine", "off")
    }
}

fn peers(i: &Inputs) -> Vec<Check> {
    if i.status.peers.is_empty() {
        return vec![Check::pass("peers", "none paired yet")];
    }
    i.status
        .peers
        .iter()
        .map(|p| {
            let name = format!("peer {}", p.name);
            match p.state {
                LinkState::Connected if p.suspect => Check::warn(
                    &name,
                    "connected, but released for silence and not heard since: its links are walls",
                    "check the network between the machines",
                ),
                LinkState::Connected => match p.rtt_us {
                    Some(r) if r > RTT_WARN_US => Check::warn(
                        &name,
                        format!(
                            "connected, round trip {:.1} ms (over 5 ms)",
                            f64::from(r) / 1000.0
                        ),
                        "check the Wi-Fi or the cable between the machines",
                    ),
                    Some(r) => Check::pass(
                        &name,
                        format!("connected, round trip {:.1} ms", f64::from(r) / 1000.0),
                    ),
                    None => Check::pass(&name, "connected"),
                },
                LinkState::Connecting => Check::warn(
                    &name,
                    "connecting",
                    "check that ferryd runs there and that the port is open",
                ),
                LinkState::Offline => Check::warn(
                    &name,
                    p.detail.clone().unwrap_or_else(|| "offline".into()),
                    "check that ferryd runs there and that the port is open",
                ),
                LinkState::Refused => Check::fail(
                    &name,
                    p.detail.clone().unwrap_or_else(|| "refused".into()),
                    "compare `ferry id` on both machines, then `ferry trust` again",
                ),
            }
        })
        .collect()
}

/// Reads the live facts and builds the checks.
pub async fn collect(ctl: &Ctl) -> DoctorResult {
    let (status, capture_on, direct, lock_budget_ms) = {
        let s = ctl.state.borrow();
        (
            s.status(),
            s.capture_on,
            s.config.input.capture == crate::config::CaptureMode::Direct,
            s.config.lock_check_budget().millis(),
        )
    };
    let (portal, rezoning_for, service) = {
        let l = ctl.live.lock();
        (
            l.portal.clone(),
            l.rezoning_since.map(|t| t.elapsed()),
            l.service.clone(),
        )
    };
    // Only the compositor ferryd found at start: a daemon without input (or started outside
    // Hyprland) says so instead of reaching for whatever else the environment names.
    let ipc = ctl.ipc.clone();
    let (hypr, globals, ufw_conf) = tokio::task::spawn_blocking(move || {
        let (hypr, globals) = match ipc {
            Some(inst) => (
                HyprFacts::read(Ok(inst)),
                ferry_wl::probe_globals(&ferry_wl::Target::default())
                    .map(|g| g.into_iter().map(|g| g.interface).collect())
                    .map_err(|e| e.to_string()),
            ),
            None => {
                let why = "input is off in this ferryd, or Hyprland was not found when it started";
                (
                    HyprFacts::unavailable(&format!("no Hyprland IPC: {why}")),
                    Err(why.to_string()),
                )
            }
        };
        let ufw = std::fs::read_to_string(UFW_CONF).ok();
        (hypr, globals, ufw)
    })
    .await
    .unwrap_or_else(|e| {
        let why = format!("the doctor task failed: {e}");
        (HyprFacts::unavailable(&why), Err(why), None)
    });
    DoctorResult {
        checks: build(&Inputs {
            status,
            capture_on,
            direct,
            portal,
            rezoning_for,
            hypr,
            globals,
            ufw_conf,
            service,
            lock_budget_ms,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ferry_core::api::{CaptureStatus, PeerRef, PeerStatus, Role};
    use ferry_core::doctor::Level;

    use super::*;

    fn status() -> StatusResult {
        StatusResult {
            api: 1,
            me: PeerRef {
                name: "desk".into(),
                fingerprint: "00".repeat(32),
            },
            port: 4243,
            role: Role::Idle,
            active_peer: None,
            capture: CaptureStatus {
                backend: "portal".into(),
                gate: "armed".into(),
                barriers: 1,
                failed_barriers: vec![],
                off_reason: None,
                notice: None,
            },
            screen_locked: Some(false),
            locked: false,
            peers: vec![],
            last_release: None,
            counters: BTreeMap::new(),
            pairing: None,
            summary: None,
            attention: vec![],
            escape_chord: None,
        }
    }

    fn healthy() -> Inputs {
        Inputs {
            status: status(),
            capture_on: true,
            direct: false,
            portal: PortalHealth {
                open: true,
                failures: 0,
                last_error: None,
            },
            rezoning_for: None,
            hypr: HyprFacts {
                version: Ok("0.56.2".into()),
                capture_bind: Ok(true),
                release_pressed_on_close: Ok(true),
                enforce_permissions: Ok(false),
            },
            globals: Ok(ferry_inject::REQUIRED_GLOBALS
                .iter()
                .map(|s| s.to_string())
                .collect()),
            ufw_conf: Some("ENABLED=no\n".into()),
            service: None,
            lock_budget_ms: 50,
        }
    }

    fn level(checks: &[Check], name: &str) -> Level {
        checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no check {name}: {checks:?}"))
            .level
    }

    fn peer(state: LinkState) -> PeerStatus {
        PeerStatus {
            name: "lap".into(),
            fingerprint: "11".repeat(32),
            state,
            addr: None,
            rtt_us: Some(800),
            proto: Some("1.0".into()),
            detail: None,
            suspect: false,
            last_seen: None,
            locked: false,
        }
    }

    #[test]
    fn a_healthy_daemon_has_no_warning() {
        let checks = build(&healthy());
        for c in &checks {
            assert_eq!(c.level, Level::Pass, "{c:?}");
        }
        assert_eq!(checks.len(), 11, "{checks:?}");
    }

    #[test]
    fn lock_to_this_machine_is_a_warn_that_names_the_way_out() {
        let mut i = healthy();
        let c = build(&i);
        let row = c.iter().find(|c| c.name == "lock to this machine").unwrap();
        assert_eq!(row.level, Level::Pass, "{row:?}");
        assert_eq!(row.detail, "off");
        i.status.locked = true;
        let c = build(&i);
        let row = c.iter().find(|c| c.name == "lock to this machine").unwrap();
        assert_eq!(row.level, Level::Warn, "{row:?}");
        assert!(row.detail.starts_with("on:"), "{row:?}");
        assert_eq!(row.fix.as_deref(), Some("ferry lock off"));
        // it is information, not a fault of the daemon: nothing else changes, and it holds
        // with capture off too
        i.capture_on = false;
        assert_eq!(level(&build(&i), "lock to this machine"), Level::Warn);
    }

    #[test]
    fn the_lock_check_row_shows_the_budget_in_effect() {
        let mut i = healthy();
        let row = |i: &Inputs| {
            build(i)
                .into_iter()
                .find(|c| c.name == "lock check")
                .unwrap()
        };
        assert!(
            row(&i).detail.contains("within the 50 ms budget"),
            "{:?}",
            row(&i)
        );
        i.lock_budget_ms = 100;
        i.status.counters.insert("lock_unknown".into(), 3);
        let r = row(&i);
        assert_eq!(r.level, Level::Warn);
        assert!(
            r.detail
                .starts_with("3 lock reads were inconclusive within the 100 ms budget"),
            "{r:?}"
        );
        assert!(
            r.fix.as_deref().unwrap().contains("lock_check_budget_ms"),
            "{r:?}"
        );
    }

    #[test]
    fn a_test_unit_is_named_with_its_watchdog() {
        let mut i = healthy();
        let unit = |i: &Inputs| build(i).into_iter().find(|c| c.name == "unit");
        // Outside a service, or under ferryd.service: the CLI's row is the one.
        assert_eq!(unit(&i), None);
        i.service = Some(Service {
            name: "ferryd.service".into(),
            watchdog: Some(Duration::from_secs(2)),
        });
        assert_eq!(unit(&i), None);
        // The exit check's transient unit (systemd-run, Type=notify, WatchdogSec=2s).
        i.service = Some(Service {
            name: "ferryd-m1exit.service".into(),
            watchdog: Some(Duration::from_secs(2)),
        });
        let row = unit(&i).unwrap();
        assert_eq!(row.level, Level::Warn);
        assert_eq!(
            row.detail,
            "ferryd runs under ferryd-m1exit.service, not ferryd.service: systemd watchdog 2 s; \
             restart and start at login are that unit's"
        );
        assert!(!row.detail.contains("outside systemd"), "{row:?}");
        i.service.as_mut().unwrap().watchdog = None;
        assert!(
            unit(&i).unwrap().detail.contains("no systemd watchdog"),
            "{:?}",
            unit(&i)
        );
    }

    #[test]
    fn an_old_or_missing_hyprland_fails() {
        let mut i = healthy();
        i.hypr.version = Ok("0.55.9".into());
        assert_eq!(level(&build(&i), "hyprland"), Level::Fail);
        i.hypr.version = Err("no Hyprland IPC: x".into());
        assert_eq!(level(&build(&i), "hyprland"), Level::Fail);
        i.hypr.version = Ok("0.57.0".into());
        assert_eq!(level(&build(&i), "hyprland"), Level::Pass);
    }

    #[test]
    fn a_denied_portal_is_not_permitted_with_the_lua_line() {
        let mut i = healthy();
        i.portal = PortalHealth {
            open: false,
            failures: 2,
            last_error: Some("portal: Permission denied".into()),
        };
        let c = build(&i);
        let cap = c.iter().find(|c| c.name == "capture").unwrap();
        assert_eq!(cap.level, Level::Fail);
        assert!(
            cap.detail.starts_with("input capture not permitted"),
            "{cap:?}"
        );
        assert!(
            cap.fix.as_deref().unwrap().contains("hl.permission("),
            "{cap:?}"
        );
        // some other failure is a retry, not a denial
        i.portal.last_error = Some("portal: no such interface".into());
        assert_eq!(level(&build(&i), "capture"), Level::Warn);
        i.capture_on = false;
        assert_eq!(level(&build(&i), "capture"), Level::Pass);
    }

    #[test]
    fn enforced_permissions_warn_until_a_crossing_has_worked() {
        let mut i = healthy();
        i.hypr.enforce_permissions = Ok(true);
        let c = build(&i);
        let cap = c.iter().find(|c| c.name == "capture").unwrap();
        assert_eq!(cap.level, Level::Warn);
        assert!(cap.fix.as_deref().unwrap().contains("input-capture"));
        // the hint offers the dialog first, then the rule and the restart it needs
        for must in [
            "answer Hyprland's permission dialog at the first crossing",
            "or allow xdph in ferry.lua and restart Hyprland",
        ] {
            assert!(cap.detail.contains(must), "{cap:?}");
        }
        let fix = cap.fix.as_deref().unwrap();
        assert!(fix.contains("then log out and back in"), "{cap:?}");
        assert!(fix.ends_with(PERMISSION_LINE), "{cap:?}");
        assert!(
            !cap.detail.contains("not enough") && !fix.contains("not enough"),
            "{cap:?}"
        );
        i.status.counters.insert("crossings".into(), 3);
        assert_eq!(level(&build(&i), "capture"), Level::Pass);
    }

    #[test]
    fn a_missing_global_fails() {
        let mut i = healthy();
        i.globals = Ok(vec!["zwp_virtual_keyboard_manager_v1".into()]);
        let c = build(&i);
        let g = c.iter().find(|c| c.name == "globals").unwrap();
        assert_eq!(g.level, Level::Fail);
        assert!(g.detail.contains("zwlr_virtual_pointer_manager_v1"));
        i.globals = Err("no display".into());
        assert_eq!(level(&build(&i), "globals"), Level::Fail);
    }

    #[test]
    fn failed_barriers_fail_and_a_stuck_rezone_warns() {
        let mut i = healthy();
        i.status.capture.gate = "barriers_failed".into();
        i.status.capture.failed_barriers = vec![1];
        assert_eq!(level(&build(&i), "barriers"), Level::Fail);
        let mut i = healthy();
        i.status.capture.gate = "rezoning".into();
        i.rezoning_for = Some(Duration::from_secs(1));
        assert_eq!(level(&build(&i), "barriers"), Level::Pass);
        i.rezoning_for = Some(Duration::from_secs(6));
        assert_eq!(level(&build(&i), "barriers"), Level::Warn);
    }

    #[test]
    fn the_escape_bind_is_a_fail_only_on_the_direct_backend() {
        let mut i = healthy();
        i.hypr.capture_bind = Ok(false);
        assert_eq!(level(&build(&i), "escape bind"), Level::Warn);
        i.direct = true;
        assert_eq!(level(&build(&i), "escape bind"), Level::Fail);
        i.hypr.capture_bind = Ok(true);
        assert_eq!(level(&build(&i), "escape bind"), Level::Pass);
    }

    #[test]
    fn release_pressed_on_close_is_a_warn_never_a_fail() {
        let mut i = healthy();
        i.hypr.release_pressed_on_close = Ok(false);
        assert_eq!(level(&build(&i), "release_pressed_on_close"), Level::Warn);
        i.hypr.release_pressed_on_close = Err("x".into());
        assert_eq!(level(&build(&i), "release_pressed_on_close"), Level::Warn);
    }

    #[test]
    fn the_firewall_reads_ufw_and_the_inbound_counter() {
        let mut i = healthy();
        i.ufw_conf = Some("ENABLED=yes\n".into());
        assert_eq!(level(&build(&i), "firewall"), Level::Warn);
        i.status.counters.insert("inbound_accepted".into(), 1);
        assert_eq!(level(&build(&i), "firewall"), Level::Pass);
        i.ufw_conf = None;
        i.status.counters.clear();
        assert_eq!(level(&build(&i), "firewall"), Level::Pass);
    }

    #[test]
    fn unknown_lock_reads_warn() {
        let mut i = healthy();
        i.status.counters.insert("lock_unknown".into(), 4);
        let c = build(&i);
        let l = c.iter().find(|c| c.name == "lock check").unwrap();
        assert_eq!(l.level, Level::Warn);
        assert!(l.detail.starts_with("4 lock reads"));
    }

    #[test]
    fn each_peer_has_its_own_row() {
        let mut i = healthy();
        let mut slow = peer(LinkState::Connected);
        slow.name = "slow".into();
        slow.rtt_us = Some(9_000);
        let mut gone = peer(LinkState::Offline);
        gone.name = "gone".into();
        let mut bad = peer(LinkState::Refused);
        bad.name = "bad".into();
        bad.detail = Some("fingerprint changed".into());
        let mut quiet = peer(LinkState::Connected);
        quiet.name = "quiet".into();
        quiet.suspect = true;
        i.status.peers = vec![peer(LinkState::Connected), slow, gone, bad, quiet];
        let c = build(&i);
        assert_eq!(level(&c, "peer lap"), Level::Pass);
        assert_eq!(level(&c, "peer slow"), Level::Warn);
        assert_eq!(level(&c, "peer gone"), Level::Warn);
        assert_eq!(level(&c, "peer bad"), Level::Fail);
        assert_eq!(level(&c, "peer quiet"), Level::Warn);
        assert!(c.iter().all(|c| c.name != "peers"));
    }
}
