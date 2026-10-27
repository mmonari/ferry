//! `ferryd.service` for a build without a package.
//!
//! The packaged unit is `dist/systemd/ferryd.service`, installed to
//! `/usr/lib/systemd/user/`. Setup writes the same file to `~/.config/systemd/user/` with
//! `ExecStart=` set to the `ferryd` beside the `ferry` that ran it, and a first line that marks
//! the file as setup's: `--undo` deletes only a file with that line.

use std::path::Path;

/// The packaged unit, as the build found it in `dist/`.
pub const TEMPLATE: &str = include_str!("../../../../dist/systemd/ferryd.service");
/// The first line of a unit setup wrote.
pub const MARKER: &str = "# Written by `ferry setup`; `ferry setup --undo` removes this file (ExecStart is this build's ferryd).";

/// One `ExecStart` word for `path`: `%` and `$` doubled (specifier and variable expansion),
/// double-quoted with `\` escapes when it holds a space, a quote or a backslash.
pub fn exec_arg(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('%', "%%").replace('$', "$$");
    if raw.contains([' ', '\t', '"', '\'', '\\']) {
        format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        raw
    }
}

/// The unit text with `ExecStart=` pointing at `ferryd`, and the marker on top.
pub fn render(ferryd: &Path) -> String {
    let mut out = format!("{MARKER}\n");
    for line in TEMPLATE.lines() {
        if line.starts_with("ExecStart=") {
            out.push_str(&format!("ExecStart={}\n", exec_arg(ferryd)));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// A unit setup wrote.
pub fn is_ours(text: &str) -> bool {
    text.lines().next() == Some(MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_rendered_unit_differs_from_the_package_only_in_execstart_and_the_marker() {
        let r = render(Path::new("/home/x/.local/bin/ferryd"));
        assert!(is_ours(&r));
        assert!(r.contains("\nExecStart=/home/x/.local/bin/ferryd\n"));
        let body: Vec<&str> = r.lines().skip(1).collect();
        let pkg: Vec<&str> = TEMPLATE.lines().collect();
        assert_eq!(body.len(), pkg.len());
        let diff: Vec<_> = body.iter().zip(&pkg).filter(|(a, b)| a != b).collect();
        assert_eq!(diff.len(), 1, "{diff:?}");
        assert!(diff[0].1.starts_with("ExecStart=/usr/bin/ferryd"));
    }

    #[test]
    fn the_unit_has_the_values_of_the_design() {
        for key in [
            "Type=notify",
            "NotifyAccess=main",
            "Restart=on-failure",
            "RestartSec=200ms",
            "RestartSteps=5",
            "RestartMaxDelaySec=10s",
            "WatchdogSec=2s",
            "TimeoutStopSec=3s",
            "NoNewPrivileges=yes",
            "LimitCORE=0",
            "UMask=0077",
            "StartLimitIntervalSec=0",
            "PartOf=graphical-session.target",
            "After=graphical-session.target",
            "WantedBy=graphical-session.target",
        ] {
            assert!(TEMPLATE.lines().any(|l| l == key), "{key}");
        }
    }

    #[test]
    fn an_exec_path_is_quoted_and_escaped_for_systemd() {
        assert_eq!(
            exec_arg(Path::new("/opt/ferry/ferryd")),
            "/opt/ferry/ferryd"
        );
        assert_eq!(
            exec_arg(&PathBuf::from("/opt/my ferry/ferryd")),
            "\"/opt/my ferry/ferryd\""
        );
        assert_eq!(exec_arg(Path::new("/a/100%/f$x")), "/a/100%%/f$$x");
        assert_eq!(exec_arg(Path::new("/a/\"b\"/f")), "\"/a/\\\"b\\\"/f\"");
    }
}
