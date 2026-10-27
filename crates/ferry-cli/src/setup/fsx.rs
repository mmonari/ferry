//! The file operations setup does to user files: follow a symlink, keep a backup, write whole
//! or not at all.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The file a path really names: a symlink (a dotfiles checkout) is followed, so an edit lands
/// in the target and the link stays a link.
pub fn resolve(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(path).unwrap_or_else(|_| {
            // A dangling link: write through to where it points.
            fs::read_link(path)
                .map(|t| path.parent().unwrap_or(Path::new("/")).join(t))
                .unwrap_or_else(|_| path.to_path_buf())
        }),
        _ => path.to_path_buf(),
    }
}

/// `<file>.ferry-bak.<unix time>`, a copy made before an edit. A second backup in the same
/// second gets `.1`, `.2`, ... so an earlier one is never overwritten.
pub fn backup(path: &Path, unix_time: u64) -> io::Result<PathBuf> {
    let name = path.file_name().unwrap_or_default().to_os_string();
    for n in 0.. {
        let mut candidate = name.clone();
        candidate.push(format!(".ferry-bak.{unix_time}"));
        if n > 0 {
            candidate.push(format!(".{n}"));
        }
        let dest = path.with_file_name(candidate);
        if !dest.exists() {
            fs::copy(path, &dest)?;
            return Ok(dest);
        }
    }
    unreachable!("an unused backup name exists")
}

/// Writes `bytes` to a temp file in the same directory and renames it over `path`, keeping the
/// mode of the file it replaces (0644 for a new one).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let mode = fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o7777)
        .unwrap_or(0o644);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".ferry-tmp.{}", std::process::id()));
    let tmp = path.with_file_name(name);
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Why a file could not be read as text.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The bytes are not UTF-8. A file setup edits is then left exactly as it is: reading it
    /// lossily and writing the text back would replace the user's bytes.
    #[error("is not valid UTF-8")]
    NotUtf8,
}

/// The file's text, or `None` when it does not exist. Never lossy: bytes that are not UTF-8 are
/// [`ReadError::NotUtf8`], so no caller can write a mangled copy back.
pub fn read_text(path: &Path) -> Result<Option<String>, ReadError> {
    match fs::read(path) {
        Ok(b) => String::from_utf8(b)
            .map(Some)
            .map_err(|_| ReadError::NotUtf8),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The file's text with invalid bytes replaced, for files that are only read (ufw's config):
/// nothing is written back from it.
pub fn read_lossy(path: &Path) -> io::Result<Option<String>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(String::from_utf8_lossy(&b).into_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn a_symlink_is_followed_and_stays_a_link() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("dotfiles/hyprland.lua");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "a = 1\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.path().join("hyprland.lua");
        symlink(&real, &link).unwrap();
        let target = resolve(&link);
        assert_eq!(target, fs::canonicalize(&real).unwrap());
        write_atomic(&target, b"a = 2\n").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&link).unwrap(), "a = 2\n");
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let b = backup(&target, 7).unwrap();
        assert!(b.to_string_lossy().ends_with("hyprland.lua.ferry-bak.7"));
        assert_eq!(fs::read_to_string(b).unwrap(), "a = 2\n");
        write_atomic(&target, b"a = 3\n").unwrap();
        let b2 = backup(&target, 7).unwrap();
        assert!(b2.to_string_lossy().ends_with("hyprland.lua.ferry-bak.7.1"));
        assert_eq!(fs::read_to_string(b2).unwrap(), "a = 3\n");
        assert_eq!(
            fs::read_to_string(target.with_file_name("hyprland.lua.ferry-bak.7")).unwrap(),
            "a = 2\n",
            "the first backup is kept"
        );
    }

    #[test]
    fn text_is_never_lossy() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("hyprland.lua");
        assert!(matches!(read_text(&p), Ok(None)));
        fs::write(&p, "-- café\n").unwrap();
        assert_eq!(read_text(&p).unwrap().as_deref(), Some("-- café\n"));
        fs::write(&p, b"-- caf\xe9\n").unwrap();
        assert!(matches!(read_text(&p), Err(ReadError::NotUtf8)));
        assert_eq!(
            read_lossy(&p).unwrap().as_deref(),
            Some("-- caf\u{fffd}\n"),
            "a file that is only read may be lossy"
        );
        assert!(matches!(read_text(d.path()), Err(ReadError::Io(_))));
    }

    #[test]
    fn a_new_file_is_0644_and_no_temp_is_left() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x/ferry.lua");
        write_atomic(&p, b"hi").unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let names: Vec<_> = fs::read_dir(p.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1);
    }
}
