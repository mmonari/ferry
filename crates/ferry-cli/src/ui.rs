//! `ferry ui <view>`: opens one of ferry's views in the running Omarchy
//! shell by calling the plugin's IPC target, `omarchy-shell ferry <function>`.
//!
//! It exists for the places that cannot name the shell's command line themselves: a launcher
//! entry (`dist/ferry.desktop`), whose environment may lack `$OMARCHY_PATH`, and a Hyprland bind.
//! It talks to the shell and not to ferryd, so it works with ferryd down (the panels then say so),
//! and it needs no home directory.

use std::io;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};

use ferry_core::api::UiView;

use crate::setup::{Env, OMARCHY_SHELL, omarchy_path_from};

/// How to reach the shell.
#[derive(Debug, Clone)]
pub struct Shell {
    /// `omarchy-shell`.
    pub program: PathBuf,
    /// `$OMARCHY_PATH` for it: the script refuses to run without one.
    pub omarchy_path: PathBuf,
}

impl Shell {
    pub fn from_env(env: &Env) -> Self {
        Self {
            program: env.omarchy_shell.clone(),
            omarchy_path: env.omarchy_path.clone(),
        }
    }

    /// For a process without a home directory, which `Env` needs.
    pub fn from_process() -> Self {
        Self {
            program: OMARCHY_SHELL.into(),
            omarchy_path: omarchy_path_from(std::env::var_os("OMARCHY_PATH").map(PathBuf::from)),
        }
    }
}

/// Why a view did not open. Each has a code for `--json`, and most a hint.
#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("omarchy-shell is not installed")]
    ShellMissing,
    #[error("the ferry plugin is not loaded in the shell (it answered: {0})")]
    PluginNotLoaded(String),
    #[error("{0}")]
    ShellNotRunning(String),
    #[error("cannot run omarchy-shell: {0}")]
    Spawn(#[source] io::Error),
    /// Whatever else it said.
    #[error("{0}")]
    Failed(String),
}

impl UiError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ShellMissing => "omarchy_shell_missing",
            Self::PluginNotLoaded(_) => "plugin_not_loaded",
            Self::ShellNotRunning(_) => "shell_not_running",
            Self::Spawn(_) | Self::Failed(_) => "ui_failed",
        }
    }

    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::ShellMissing => Some(
                "the ferry panels are a plugin for the Omarchy shell; other frontends read ferryd's socket instead",
            ),
            Self::PluginNotLoaded(_) => {
                Some("run ferry setup, which links and enables the ferry.bar plugin")
            }
            Self::ShellNotRunning(_) => Some("start the shell, then try again"),
            Self::Spawn(_) | Self::Failed(_) => None,
        }
    }
}

/// The plugin's IPC function that shows `view` (`plugin/FerryUi.qml`).
fn function_of(view: UiView) -> Option<&'static str> {
    match view {
        UiView::Arrange => Some("arrange"),
        UiView::Pair => Some("pair"),
        UiView::Settings => Some("settings"),
        UiView::Popover => Some("open"),
        UiView::Unknown => None,
    }
}

/// What `omarchy-shell` said when it failed, as a [`UiError`]. Its messages are plain lines on
/// its stderr: `Target not found.` is what `qs ipc` answers for an IPC target nothing registered.
fn classify(said: &str, status: ExitStatus) -> UiError {
    let said = said.trim();
    if said.contains("Target not found") {
        return UiError::PluginNotLoaded(said.to_string());
    }
    if ["is not running", "is not responding", "is not ready"]
        .iter()
        .any(|m| said.contains(&format!("omarchy-shell {m}")))
    {
        return UiError::ShellNotRunning(said.to_string());
    }
    if said.is_empty() {
        return UiError::Failed(format!("omarchy-shell exited with {status}"));
    }
    UiError::Failed(said.to_string())
}

/// Shows `view`. Nothing is printed on success.
pub fn open(view: UiView, shell: &Shell) -> Result<(), UiError> {
    let Some(function) = function_of(view) else {
        return Err(UiError::Failed(format!(
            "this ferry does not know the view {:?}",
            view.as_str()
        )));
    };
    let output = Command::new(&shell.program)
        .args(["ferry", function])
        .env("OMARCHY_PATH", &shell.omarchy_path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                UiError::ShellMissing
            } else {
                UiError::Spawn(e)
            }
        })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let said = if stderr.trim().is_empty() {
        String::from_utf8_lossy(&output.stdout)
    } else {
        stderr
    };
    Err(classify(&said, output.status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn every_view_has_its_function() {
        assert_eq!(function_of(UiView::Arrange), Some("arrange"));
        assert_eq!(function_of(UiView::Pair), Some("pair"));
        assert_eq!(function_of(UiView::Settings), Some("settings"));
        assert_eq!(function_of(UiView::Popover), Some("open"));
        assert_eq!(function_of(UiView::Unknown), None);
    }

    #[test]
    fn what_the_shell_says_is_what_went_wrong() {
        let status = ExitStatus::from_raw(1 << 8);
        let code = |said: &str| classify(said, status).code();
        assert_eq!(code("Target not found."), "plugin_not_loaded");
        assert_eq!(code("Target not found.\n"), "plugin_not_loaded");
        for said in [
            "omarchy-shell is not running",
            "omarchy-shell is not responding",
            "omarchy-shell is not ready",
        ] {
            assert_eq!(code(said), "shell_not_running", "{said}");
        }
        assert_eq!(code("Function not found."), "ui_failed");
        assert_eq!(code(""), "ui_failed");
        assert!(classify("", status).to_string().contains("exited with"));
        assert_eq!(classify("Function not found.", status).hint(), None);
        // a plugin that is "not running" is not the shell
        assert_eq!(code("the ferry plugin is not running"), "ui_failed");
    }
}
