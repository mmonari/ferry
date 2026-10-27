//! Why ferryd would not start. Every message says what to do next.

use std::path::PathBuf;

use ferry_net::IdentityError;

use crate::fsutil::StoreError;
use crate::paths::PathsError;

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Paths(#[from] PathsError),
    #[error("config: {0}")]
    Config(#[source] StoreError),
    #[error("identity: {0}")]
    Identity(#[from] IdentityError),
    /// `trusted.toml` or `layout.toml`.
    #[error("{0}")]
    Store(#[source] StoreError),
    #[error("ferryd is already running: {} answers (stop it first, or this is the instance you wanted)", .0.display())]
    AlreadyRunning(PathBuf),
    #[error("socket {}: {source}", path.display())]
    Socket {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("network endpoint: {0}")]
    Net(String),
    #[error("cannot start the {name} thread: {source}")]
    Thread {
        name: &'static str,
        #[source]
        source: std::io::Error,
    },
}
