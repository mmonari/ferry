//! What can go wrong inside `ferry setup`, by cause. A step still reports its
//! failure as text (`Out::Failed`), built from these; the variants are what tests and callers
//! branch on, so "no Hyprland running" and "Hyprland answered garbage" stay apart.

use std::io;
use std::path::PathBuf;

use ferry_core::chord::ChordError;

use super::fsx::ReadError;

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    /// The same rule and text as ferryd's `PathsError::NoHome`: without a home directory
    /// the CLI would read and write `/.config`.
    #[error("HOME is not set, and neither is XDG_CONFIG_HOME, XDG_DATA_HOME or XDG_STATE_HOME")]
    NoHome,
    #[error("no XDG_RUNTIME_DIR")]
    NoRuntimeDir,
    /// No Hyprland instance, or its socket did not answer.
    #[error("{0}")]
    Hyprland(#[from] ferry_hypr_ipc::Error),
    /// Hyprland answered, and the answer is not JSON.
    #[error("{what}: {source}")]
    BadAnswer {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },
    /// Hyprland answered JSON that does not hold what was asked for.
    #[error("{what}: {detail}")]
    UnexpectedAnswer { what: &'static str, detail: String },
    /// A file could not be read, or is not UTF-8.
    #[error("{}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: ReadError,
    },
    #[error("{}: {source}", path.display())]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{}: escape_chord: {source}", path.display())]
    EscapeChord {
        path: PathBuf,
        #[source]
        source: ChordError,
    },
    #[error("{}: escape_chord is not a string", .0.display())]
    EscapeChordNotText(PathBuf),
    #[error("{}: go_chord: {source}", path.display())]
    GoChord {
        path: PathBuf,
        #[source]
        source: ChordError,
    },
    #[error("{}: go_chord is not a string", .0.display())]
    GoChordNotText(PathBuf),
    /// The program could not be started.
    #[error("{}: {source}", program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The program ran and exited with a failure.
    #[error("exit {code}: {stderr}")]
    CommandFailed { code: i32, stderr: String },
}
