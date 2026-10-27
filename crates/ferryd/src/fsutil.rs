//! Atomic file writes and mode checks for ferryd's stores.

use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A store file could not be read, parsed or written. The text names the file.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The file exists but is not valid TOML for this store. ferryd never overwrites it.
    #[error(
        "{path} is corrupted: {message}\n  fix the file or move it away; ferryd will not overwrite it"
    )]
    Corrupt { path: PathBuf, message: String },
}

impl StoreError {
    pub(crate) fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn corrupt(path: &Path, message: impl std::fmt::Display) -> Self {
        Self::Corrupt {
            path: path.to_path_buf(),
            message: message.to_string(),
        }
    }
}

/// Writes `bytes` to `path` with `mode`: into a temp file in the same directory, flushed to
/// disk, then renamed over `path`. A reader sees the old file or the new one, never a torn one.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), StoreError> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let go = || -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        // NamedTempFile creates the file 0600, so a 0600 target is never visible with wider bits.
        let mut tmp = tempfile::Builder::new()
            .prefix(".ferry-")
            .suffix(".tmp")
            .tempfile_in(dir)?;
        tmp.write_all(bytes)?;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|e| e.error)?;
        // Make the rename itself durable.
        std::fs::File::open(dir)?.sync_all()
    };
    go().map_err(|e| StoreError::io(path, e))
}

/// Reads `path`; a missing file is `Ok(None)`.
pub fn read_optional(path: &Path) -> Result<Option<String>, StoreError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::io(path, e)),
    }
}

/// The permission bits of `path`.
pub fn mode_of(path: &Path) -> io::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o7777)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_with_the_mode_and_replaces_whole() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/f.toml");
        write_atomic(&p, b"one", 0o600).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        assert_eq!(mode_of(&p).unwrap(), 0o600);
        write_atomic(&p, b"two-longer", 0o644).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two-longer");
        assert_eq!(mode_of(&p).unwrap(), 0o644);
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        write_atomic(&p, b"x", 0o600).unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("f")]);
    }

    #[test]
    fn a_failed_write_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        write_atomic(&p, b"old", 0o600).unwrap();
        // The target is a directory now: the rename fails, the temp file must not linger.
        std::fs::remove_file(&p).unwrap();
        std::fs::create_dir(&p).unwrap();
        assert!(write_atomic(&p, b"new", 0o600).is_err());
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_optional(&dir.path().join("nope")).unwrap(), None);
    }
}
