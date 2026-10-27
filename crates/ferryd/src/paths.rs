//! Where ferryd keeps things.
//!
//! | What | Where |
//! |---|---|
//! | `config.toml` | `$XDG_CONFIG_HOME/ferry/` (`~/.config/ferry/`) |
//! | key, cert, `trusted.toml` | `$XDG_DATA_HOME/ferry/` (`~/.local/share/ferry/`) |
//! | `layout.toml`, `state.toml` | `$XDG_STATE_HOME/ferry/` (`~/.local/state/ferry/`) |
//! | the socket | `$XDG_RUNTIME_DIR/ferry/ferryd.sock`, or `/run/user/<uid>/ferry/` |

use std::path::{Path, PathBuf};

/// Every directory ferryd reads or writes. Tests build one over temp dirs with
/// [`Paths::under_home`], so nothing touches the real `~/.config/ferry` and friends.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    /// Holds `key.der`, `cert.der` and `trusted.toml`. Created 0700 by ferry-net.
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("HOME is not set, and neither is XDG_CONFIG_HOME, XDG_DATA_HOME or XDG_STATE_HOME")]
    NoHome,
}

/// The environment variables that decide the paths, as plain values so the rules are testable.
#[derive(Debug, Default, Clone)]
pub struct PathEnv {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub xdg_runtime_dir: Option<PathBuf>,
}

impl PathEnv {
    pub fn from_process() -> Self {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
        Self {
            home: var("HOME"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            xdg_data_home: var("XDG_DATA_HOME"),
            xdg_state_home: var("XDG_STATE_HOME"),
            xdg_runtime_dir: var("XDG_RUNTIME_DIR"),
        }
    }
}

/// The XDG Base Directory spec ignores a relative path in these variables.
fn absolute(p: Option<PathBuf>) -> Option<PathBuf> {
    p.filter(|p| p.is_absolute())
}

/// `$XDG_RUNTIME_DIR`, or `/run/user/<uid>` (architecture §7: a non-session start has no variable).
pub fn runtime_dir(var: Option<PathBuf>) -> PathBuf {
    absolute(var).unwrap_or_else(|| {
        PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
    })
}

impl Paths {
    pub fn from_env() -> Result<Self, PathsError> {
        Self::resolve(PathEnv::from_process())
    }

    pub fn resolve(env: PathEnv) -> Result<Self, PathsError> {
        let home = absolute(env.home);
        let under = |var: Option<PathBuf>, rel: &str| -> Result<PathBuf, PathsError> {
            match absolute(var) {
                Some(dir) => Ok(dir),
                None => Ok(home.as_ref().ok_or(PathsError::NoHome)?.join(rel)),
            }
        };
        Ok(Self {
            config_file: under(env.xdg_config_home, ".config")?.join("ferry/config.toml"),
            data_dir: under(env.xdg_data_home, ".local/share")?.join("ferry"),
            state_dir: under(env.xdg_state_home, ".local/state")?.join("ferry"),
            runtime_dir: runtime_dir(env.xdg_runtime_dir),
        })
    }

    /// The default layout under a given `HOME` and runtime dir (what a test with a temp `HOME`
    /// and a temp `XDG_RUNTIME_DIR` gets).
    pub fn under_home(home: &Path, runtime_dir: &Path) -> Self {
        Self::resolve(PathEnv {
            home: Some(home.to_path_buf()),
            xdg_runtime_dir: Some(runtime_dir.to_path_buf()),
            ..PathEnv::default()
        })
        .expect("an absolute HOME resolves")
    }

    pub fn socket_dir(&self) -> PathBuf {
        self.runtime_dir.join("ferry")
    }

    pub fn socket_path(&self) -> PathBuf {
        self.socket_dir().join("ferryd.sock")
    }

    pub fn trusted_file(&self) -> PathBuf {
        self.data_dir.join("trusted.toml")
    }

    pub fn layout_file(&self) -> PathBuf {
        self.state_dir.join("layout.toml")
    }

    /// This peer's own state: lock to this machine.
    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("state.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_home() {
        let p = Paths::under_home(Path::new("/h"), Path::new("/run/x"));
        assert_eq!(p.config_file, Path::new("/h/.config/ferry/config.toml"));
        assert_eq!(p.data_dir, Path::new("/h/.local/share/ferry"));
        assert_eq!(p.state_dir, Path::new("/h/.local/state/ferry"));
        assert_eq!(p.socket_path(), Path::new("/run/x/ferry/ferryd.sock"));
        assert_eq!(
            p.trusted_file(),
            Path::new("/h/.local/share/ferry/trusted.toml")
        );
        assert_eq!(
            p.layout_file(),
            Path::new("/h/.local/state/ferry/layout.toml")
        );
    }

    #[test]
    fn xdg_overrides_win_when_absolute() {
        let p = Paths::resolve(PathEnv {
            home: Some("/h".into()),
            xdg_config_home: Some("/c".into()),
            xdg_data_home: Some("relative/ignored".into()),
            xdg_state_home: Some("/s".into()),
            xdg_runtime_dir: Some("/r".into()),
        })
        .unwrap();
        assert_eq!(p.config_file, Path::new("/c/ferry/config.toml"));
        assert_eq!(p.data_dir, Path::new("/h/.local/share/ferry"));
        assert_eq!(p.state_dir, Path::new("/s/ferry"));
        assert_eq!(p.runtime_dir, Path::new("/r"));
    }

    #[test]
    fn runtime_dir_falls_back_to_run_user_uid() {
        let uid = rustix::process::getuid().as_raw();
        assert_eq!(runtime_dir(None), PathBuf::from(format!("/run/user/{uid}")));
        assert_eq!(
            runtime_dir(Some("rel".into())),
            PathBuf::from(format!("/run/user/{uid}"))
        );
    }

    #[test]
    fn no_home_and_no_xdg_is_an_error() {
        assert!(matches!(
            Paths::resolve(PathEnv::default()),
            Err(PathsError::NoHome)
        ));
    }
}
