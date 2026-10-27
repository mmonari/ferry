//! `$XDG_STATE_HOME/ferry/state.toml`: this peer's own state that outlives a restart (design
//! m1-rest §5). In M1 that is lock to this machine only:
//!
//! ```toml
//! locked = true
//! ```
//!
//! A missing file is the default (unlocked). An unknown key is ignored, so a file written by a
//! newer ferry does not stop an older ferryd. A file that is not TOML stops the start, as the
//! other stores do: ferryd never overwrites it.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fsutil::{StoreError, read_optional, write_atomic};

/// The file's mode: private, like every ferryd store.
const MODE: u32 = 0o600;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StateFile {
    /// Lock to this machine.
    pub locked: bool,
}

impl StateFile {
    pub fn load(path: &Path) -> Result<Self, StoreError> {
        match read_optional(path)? {
            Some(text) => toml::from_str(&text).map_err(|e| StoreError::corrupt(path, e.message())),
            None => Ok(Self::default()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        let text = toml::to_string(self).map_err(|e| StoreError::corrupt(path, e))?;
        write_atomic(path, text.as_bytes(), MODE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::mode_of;

    #[test]
    fn missing_is_unlocked_and_a_save_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ferry/state.toml");
        assert_eq!(StateFile::load(&p).unwrap(), StateFile { locked: false });
        StateFile { locked: true }.save(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "locked = true\n");
        assert_eq!(mode_of(&p).unwrap(), 0o600);
        assert_eq!(StateFile::load(&p).unwrap(), StateFile { locked: true });
    }

    #[test]
    fn unknown_keys_are_ignored_and_garbage_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.toml");
        std::fs::write(&p, "locked = true\nclipboard = false\n").unwrap();
        assert!(StateFile::load(&p).unwrap().locked);
        std::fs::write(&p, "locked = 'yes please'\n").unwrap();
        assert!(matches!(
            StateFile::load(&p),
            Err(StoreError::Corrupt { .. })
        ));
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// `state.toml` is a serde wrapper, so this is `should` (G-3): any bytes on disk load
        /// to a value or a `Corrupt` error, and a saved value loads back.
        #[test]
        fn state_file_load_never_panics_and_save_round_trips(
            bytes in proptest::collection::vec(any::<u8>(), 0..200),
            locked in any::<bool>(),
        ) {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("state.toml");
            std::fs::write(&p, &bytes).unwrap();
            let _ = StateFile::load(&p);
            StateFile { locked }.save(&p).unwrap();
            prop_assert_eq!(StateFile::load(&p).unwrap(), StateFile { locked });
        }
    }
}
