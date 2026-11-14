//! File helpers that do not depend on OCPP: the durable atomic write used by the state store.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// A failed [`write_atomic`]: the path that the failing step worked on, and why.
#[derive(Debug, Error)]
#[error("{}: {source}", path.display())]
pub struct WriteError {
    /// The target file, or its directory when creating or syncing the directory failed.
    pub path: PathBuf,
    pub source: io::Error,
}

/// Atomically replaces `path` with `bytes`, creating its directory if needed.
///
/// The bytes go to a temporary file in the same directory, are synced, and are renamed over
/// `path`; the directory is synced afterwards. A reader sees the old or the new content, never
/// a mix, and no temporary file is left behind on failure. The new file is private to the user
/// on Unix (the temporary file's mode is kept).
///
/// # Errors
/// [`WriteError`] naming the path of the step that failed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let fail = |at: &Path, source: io::Error| WriteError {
        path: at.to_owned(),
        source,
    };
    fs::create_dir_all(dir).map_err(|e| fail(dir, e))?;
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|e| fail(path, e))?;
    temp.write_all(bytes).map_err(|e| fail(path, e))?;
    temp.as_file().sync_all().map_err(|e| fail(path, e))?;
    temp.persist(path).map_err(|e| fail(path, e.error))?;
    match sync_dir(dir) {
        Ok(()) => Ok(()),
        // The rename already happened; some filesystems cannot sync a directory.
        Err(e) if dir_sync_unsupported(&e) => Ok(()),
        Err(e) => Err(fail(dir, e)),
    }
}

/// Whether a directory `fsync` failed only because the filesystem does not support it.
pub(crate) fn dir_sync_unsupported(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
    )
}

/// Makes the rename durable. Directories cannot be opened and synced on Windows (and NTFS
/// journals the rename), so it is a no-op there.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "same signature as the unix version"
)]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("file.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["file.json"], "no temporary file is left behind");
    }

    #[test]
    fn a_failure_names_the_path_it_worked_on() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("plain-file");
        fs::write(&blocker, b"x").unwrap();
        // The "directory" of the target is a regular file.
        let error = write_atomic(&blocker.join("file.json"), b"x").unwrap_err();
        assert_eq!(error.path, blocker);
        assert!(
            error
                .to_string()
                .starts_with(&blocker.display().to_string())
        );
    }

    #[test]
    fn unsupported_directory_sync_is_not_a_failure() {
        for kind in [io::ErrorKind::Unsupported, io::ErrorKind::InvalidInput] {
            assert!(dir_sync_unsupported(&io::Error::from(kind)));
        }
        assert!(!dir_sync_unsupported(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }
}
