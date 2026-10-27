//! The firewall step: ufw, which Omarchy turns on with incoming denied.

use std::path::Path;

pub use ferry_core::doctor::ufw_enabled;

/// The command that opens the port: the packaged `ferry` application profile when it is
/// installed, else a plain rule.
pub fn allow_command(profiles: &[&Path], port: u16) -> Vec<String> {
    let have_profile = profiles.iter().any(|p| p.is_file());
    if have_profile {
        ["ufw", "allow", "ferry"].map(str::to_string).to_vec()
    } else {
        vec![
            "ufw".into(),
            "allow".into(),
            format!("{port}/udp"),
            "comment".into(),
            "ferry".into(),
        ]
    }
}

/// The command that takes the rule away again, printed by `--undo` (setup never runs sudo for it).
pub fn delete_command(profiles: &[&Path], port: u16) -> Vec<String> {
    let rule = if profiles.iter().any(|p| p.is_file()) {
        "ferry".to_string()
    } else {
        format!("{port}/udp")
    };
    ["ufw", "delete", "allow"]
        .map(str::to_string)
        .into_iter()
        .chain([rule])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_uses_the_profile_when_it_is_installed() {
        let d = tempfile::tempdir().unwrap();
        let prof = d.path().join("ferry");
        assert_eq!(
            allow_command(&[&prof], 4243).join(" "),
            "ufw allow 4243/udp comment ferry"
        );
        std::fs::write(&prof, "[ferry]\n").unwrap();
        assert_eq!(allow_command(&[&prof], 4243).join(" "), "ufw allow ferry");
        assert_eq!(
            delete_command(&[&prof], 4243).join(" "),
            "ufw delete allow ferry"
        );
        let none = d.path().join("none");
        assert_eq!(
            delete_command(&[&none], 4243).join(" "),
            "ufw delete allow 4243/udp"
        );
    }
}
