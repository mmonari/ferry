//! Reading Hyprland's live state through its IPC socket (read-only), and applying an edit to
//! its config and waiting for the reload. Setup decides from the live state, never from parsing
//! Lua.

use std::time::{Duration, Instant};

use ferry_hypr_ipc::Instance;
use serde_json::Value;

use ferry_core::chord::{ARROWS, GoChord, Modifier};

use super::error::SetupError;
use super::lua::{GO_DESCRIPTION, go_name};

/// `hyprctl binds -j` holds a bind with `allow_input_capture`. A Lua bind's `dispatcher` is
/// `__lua`, so what it does cannot be read back; the flag is what makes it an escape bind.
pub fn has_capture_bind(inst: &Instance) -> Result<bool, SetupError> {
    let text = inst.request("j/binds")?;
    binds_have_capture_bind(&text)
}

pub fn binds_have_capture_bind(json: &str) -> Result<bool, SetupError> {
    let v: Value = serde_json::from_str(json).map_err(|source| SetupError::BadAnswer {
        what: "binds",
        source,
    })?;
    let binds = v.as_array().ok_or_else(|| SetupError::UnexpectedAnswer {
        what: "binds",
        detail: "not a list".into(),
    })?;
    Ok(binds
        .iter()
        .any(|b| b["allow_input_capture"].as_bool() == Some(true)))
}

/// `hyprctl binds -j` holds the four go binds with the go chord's modifiers: found by
/// their description, since a Lua bind's dispatcher reads `__lua`.
pub fn has_go_binds(inst: &Instance, go: &GoChord) -> Result<bool, SetupError> {
    let text = inst.request("j/binds")?;
    binds_have_go_binds(&text, go)
}

pub fn binds_have_go_binds(json: &str, go: &GoChord) -> Result<bool, SetupError> {
    let v: Value = serde_json::from_str(json).map_err(|source| SetupError::BadAnswer {
        what: "binds",
        source,
    })?;
    let binds = v.as_array().ok_or_else(|| SetupError::UnexpectedAnswer {
        what: "binds",
        detail: "not a list".into(),
    })?;
    let mask = modmask(go);
    Ok(ARROWS.iter().all(|(_, side)| {
        let description = format!("{GO_DESCRIPTION}{}", go_name(*side));
        binds.iter().any(|b| {
            b["description"].as_str() == Some(description.as_str())
                && b["modmask"].as_u64() == Some(mask)
        })
    }))
}

/// Hyprland's modifier mask: Shift 1, Ctrl 4, Alt 8, Super 64.
fn modmask(go: &GoChord) -> u64 {
    go.modifiers
        .iter()
        .map(|m| match m {
            Modifier::Shift => 1,
            Modifier::Ctrl => 4,
            Modifier::Alt => 8,
            Modifier::Super => 64,
        })
        .sum()
}

/// `hyprctl getoption <name> -j` reads true. A boolean option answers `bool`, an integer one
/// `int`.
pub fn option_on(inst: &Instance, name: &str) -> Result<bool, SetupError> {
    let text = inst.request(&format!("j/getoption {name}"))?;
    option_value_on(&text)
}

pub fn option_value_on(json: &str) -> Result<bool, SetupError> {
    let v: Value = serde_json::from_str(json).map_err(|source| SetupError::BadAnswer {
        what: "getoption",
        source,
    })?;
    if let Some(b) = v["bool"].as_bool() {
        return Ok(b);
    }
    if let Some(i) = v["int"].as_i64() {
        return Ok(i != 0);
    }
    Err(SetupError::UnexpectedAnswer {
        what: "getoption",
        detail: format!("no value in {json}"),
    })
}

/// `hyprctl configerrors`: empty when the config loaded cleanly.
pub fn config_errors(inst: &Instance) -> Result<String, SetupError> {
    Ok(inst.request("configerrors")?.trim().to_string())
}

/// What happened after an edit.
#[derive(Debug, PartialEq, Eq)]
pub enum Reload {
    /// The live state is what the edit meant.
    Live,
    /// Hyprland loaded the config without new errors, but the state does not show it.
    NotSeen,
    /// Hyprland reported errors the config did not have before.
    Errors(String),
}

/// Waits up to `wait` for Hyprland's auto-reload to show `is_live`; if it has not, asks for a
/// reload once and waits again. New config errors end the wait at once.
pub fn wait_for_reload(
    inst: &Instance,
    errors_before: &str,
    wait: Duration,
    mut is_live: impl FnMut() -> Result<bool, SetupError>,
) -> Reload {
    let poll = |until: Instant, is_live: &mut dyn FnMut() -> Result<bool, SetupError>| loop {
        if let Ok(errs) = config_errors(inst)
            && !errs.is_empty()
            && errs != errors_before
        {
            return Some(Reload::Errors(errs));
        }
        if is_live().unwrap_or(false) {
            return Some(Reload::Live);
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if let Some(r) = poll(Instant::now() + wait, &mut is_live) {
        return r;
    }
    // Auto-reload may be off, or may not watch a file that was only required.
    let _ = inst.request("reload");
    poll(Instant::now() + wait, &mut is_live).unwrap_or(Reload::NotSeen)
}

/// After a rollback: asks Hyprland to reload the restored files, and waits up to `wait` until it
/// reports no errors beyond `errors_before` and `restored` shows the state from before the edit.
/// `false` when Hyprland did not settle in time; the files are restored either way.
pub fn reload_restored(
    inst: &Instance,
    errors_before: &str,
    wait: Duration,
    mut restored: impl FnMut() -> Result<bool, SetupError>,
) -> bool {
    let _ = inst.request("reload");
    let until = Instant::now() + wait;
    loop {
        let clean = config_errors(inst).is_ok_and(|e| e.is_empty() || e == errors_before);
        if clean && restored().unwrap_or(false) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_go_binds_are_found_by_description_and_mask() {
        let go = GoChord::default();
        let bind = |side: &str, mask: u64| {
            format!(
                r#"{{"key":"X","modmask":{mask},"description":"ferry: go {side}","dispatcher":"__lua"}}"#
            )
        };
        let all: Vec<String> = ["left", "right", "up", "down"]
            .iter()
            .map(|s| bind(s, 76))
            .collect();
        assert!(binds_have_go_binds(&format!("[{}]", all.join(",")), &go).unwrap());
        // Three of four, or the old chord's mask, is not done.
        assert!(!binds_have_go_binds(&format!("[{}]", all[..3].join(",")), &go).unwrap());
        let old: Vec<String> = ["left", "right", "up", "down"]
            .iter()
            .map(|s| bind(s, 68))
            .collect();
        assert!(!binds_have_go_binds(&format!("[{}]", old.join(",")), &go).unwrap());
    }

    #[test]
    fn a_bind_with_the_flag_is_an_escape_bind() {
        let some = r#"[{"key":"A","allow_input_capture":false},{"key":"ESCAPE","modmask":68,"allow_input_capture":true,"dispatcher":"__lua"}]"#;
        assert!(binds_have_capture_bind(some).unwrap());
        let none = r#"[{"key":"A","allow_input_capture":false},{"key":"B"}]"#;
        assert!(!binds_have_capture_bind(none).unwrap());
        assert!(matches!(
            binds_have_capture_bind("nope"),
            Err(SetupError::BadAnswer { what: "binds", .. })
        ));
        assert!(matches!(
            binds_have_capture_bind("{}"),
            Err(SetupError::UnexpectedAnswer { what: "binds", .. })
        ));
    }

    #[test]
    fn options_read_as_bool_or_int() {
        assert!(option_value_on(r#"{"option":"x","bool":true,"set":true}"#).unwrap());
        assert!(!option_value_on(r#"{"option":"x","bool":false,"set":false}"#).unwrap());
        assert!(option_value_on(r#"{"option":"x","int":1,"set":true}"#).unwrap());
        assert!(!option_value_on(r#"{"option":"x","int":0}"#).unwrap());
        assert!(matches!(
            option_value_on(r#"{"option":"x","str":"a"}"#),
            Err(SetupError::UnexpectedAnswer {
                what: "getoption",
                ..
            })
        ));
        assert!(matches!(
            option_value_on("nope"),
            Err(SetupError::BadAnswer {
                what: "getoption",
                ..
            })
        ));
    }
}
