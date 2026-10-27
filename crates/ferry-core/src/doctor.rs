//! `ferry doctor`'s result shapes: one list of checks, each
//! `pass`, `warn` or `fail`, with a detail line and, when there is something to do, a fix line.
//! ferryd answers the `doctor` socket method with the checks only it can make; the CLI adds its
//! own and prints the merged list, so the M3 panel can show the same one.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Pass,
    Warn,
    Fail,
}

impl Level {
    /// `PASS`, `WARN`, `FAIL`.
    pub fn label(self) -> &'static str {
        match self {
            Level::Pass => "PASS",
            Level::Warn => "WARN",
            Level::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// A short fixed name: `hyprland`, `escape bind`, `peers`.
    pub name: String,
    pub level: Level,
    pub detail: String,
    /// What to run or change; absent on a pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    pub fn new(name: &str, level: Level, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            level,
            detail: detail.into(),
            fix: None,
        }
    }

    pub fn pass(name: &str, detail: impl Into<String>) -> Self {
        Self::new(name, Level::Pass, detail)
    }

    pub fn warn(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::new(name, Level::Warn, detail).with_fix(fix)
    }

    pub fn fail(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::new(name, Level::Fail, detail).with_fix(fix)
    }

    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        let fix = fix.into();
        self.fix = (!fix.is_empty()).then_some(fix);
        self
    }
}

/// The `doctor` method's result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DoctorResult {
    pub checks: Vec<Check>,
}

impl DoctorResult {
    pub fn count(&self, level: Level) -> usize {
        self.checks.iter().filter(|c| c.level == level).count()
    }
}

/// `ENABLED=yes` in `/etc/ufw/ufw.conf` (world-readable, so no sudo).
pub fn ufw_enabled(conf: &str) -> bool {
    conf.lines()
        .filter_map(|l| l.split_once('='))
        .any(|(k, v)| k.trim() == "ENABLED" && v.trim().eq_ignore_ascii_case("yes"))
}

/// The firewall check: ufw's config, and how many inbound connections ferryd has accepted since
/// it started (`None`: ferryd is not there to say).
pub fn firewall_check(ufw_conf: Option<&str>, port: u16, inbound_accepted: Option<u64>) -> Check {
    if !ufw_conf.is_some_and(ufw_enabled) {
        return Check::pass("firewall", "ufw is off");
    }
    let fix = format!("sudo ufw allow {port}/udp   (only if a peer must dial this machine)");
    match inbound_accepted {
        Some(n) if n > 0 => Check::pass("firewall", "ufw is on and a peer has connected in"),
        Some(_) => Check::warn(
            "firewall",
            "ufw is on and no peer has connected in yet",
            fix,
        ),
        None => Check::warn(
            "firewall",
            "ufw is on, and ferryd is not running to say whether a peer has connected in",
            fix,
        ),
    }
}

/// `0.56.2` (or `v0.56.2`, `0.56.2-4`) against a minimum, as numbers.
pub fn version_at_least(have: &str, min: (u32, u32, u32)) -> Option<bool> {
    let core = have.trim().trim_start_matches('v');
    let core = core.split(['-', '+', ' ']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u32>().ok());
    let v = (
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    );
    Some(v >= min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(version_at_least("0.56.2", (0, 56, 2)), Some(true));
        assert_eq!(version_at_least("v0.56.10", (0, 56, 2)), Some(true));
        assert_eq!(version_at_least("0.56.1", (0, 56, 2)), Some(false));
        assert_eq!(version_at_least("0.9.9", (0, 56, 2)), Some(false));
        assert_eq!(version_at_least("1.4.1-2", (1, 4, 1)), Some(true));
        assert_eq!(version_at_least("1.4", (1, 4, 1)), Some(false));
        assert_eq!(version_at_least("git", (1, 4, 1)), None);
    }

    #[test]
    fn a_check_serialises_without_an_empty_fix() {
        let c = Check::pass("daemon", "ferryd 0.1.0, api 1");
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"name":"daemon","level":"pass","detail":"ferryd 0.1.0, api 1"}"#
        );
        let c = Check::warn("firewall", "ufw is on", "sudo ufw allow 4243/udp");
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"name":"firewall","level":"warn","detail":"ufw is on","fix":"sudo ufw allow 4243/udp"}"#
        );
        assert!(Check::warn("x", "y", "").fix.is_none());
    }

    #[test]
    fn ufw_is_on_only_when_the_config_says_so() {
        assert!(ufw_enabled("# c\nENABLED=yes\nLOGLEVEL=low\n"));
        assert!(ufw_enabled("ENABLED = YES\n"));
        assert!(!ufw_enabled("ENABLED=no\n"));
        assert!(!ufw_enabled("# ENABLED=yes\n"));
        assert!(!ufw_enabled(""));
    }

    #[test]
    fn the_firewall_warns_only_when_ufw_is_on_and_nothing_got_in() {
        let on = Some("ENABLED=yes\n");
        assert_eq!(firewall_check(None, 4243, Some(0)).level, Level::Pass);
        assert_eq!(
            firewall_check(Some("ENABLED=no\n"), 4243, None).level,
            Level::Pass
        );
        let c = firewall_check(on, 4243, Some(0));
        assert_eq!(c.level, Level::Warn);
        assert!(c.fix.unwrap().starts_with("sudo ufw allow 4243/udp"));
        assert_eq!(firewall_check(on, 4243, Some(2)).level, Level::Pass);
        let c = firewall_check(on, 4243, None);
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains("ferryd is not running"));
    }

    #[test]
    fn levels_order_by_severity() {
        assert!(Level::Pass < Level::Warn && Level::Warn < Level::Fail);
    }
}
