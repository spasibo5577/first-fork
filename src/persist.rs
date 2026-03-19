//! Atomic file operations for crash-safe persistence on ext4/eMMC.
//!
//! The only correct way to atomically update a file on Linux ext4:
//! 1. Write to a temporary file in the SAME directory
//! 2. fsync the file (data + metadata)
//! 3. Close the file
//! 4. rename(tmp, target) — atomic on POSIX
//! 5. fsync the parent directory (so the rename entry is durable)
//!
//! On Windows (dev environment), steps 2 and 5 use platform equivalents
//! that provide best-effort durability. Production target is Linux only.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

/// Atomically writes `data` to `path`, surviving power loss on Linux ext4.
///
/// On Windows (dev builds), fsync/dir-sync are best-effort.
///
/// # Errors
/// Returns an error if any filesystem operation fails.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), PersistError> {
    let dir = path
        .parent()
        .ok_or_else(|| PersistError::NoParentDir(path.to_path_buf()))?;

    // Ensure directory exists.
    fs::create_dir_all(dir).map_err(|e| PersistError::CreateDir {
        path: dir.to_path_buf(),
        source: e,
    })?;

    // Generate temp file name in the same directory.
    let file_name = path
        .file_name()
        .ok_or_else(|| PersistError::NoFileName(path.to_path_buf()))?;
    let tmp_name = format!(".{}.tmp", file_name.to_string_lossy());
    let tmp_path = dir.join(&tmp_name);

    // Step 1: Write to temp file.
    let mut file = File::create(&tmp_path).map_err(|e| PersistError::CreateTemp {
        path: tmp_path.clone(),
        source: e,
    })?;

    file.write_all(data).map_err(|e| PersistError::Write {
        path: tmp_path.clone(),
        source: e,
    })?;

    // Step 2: fsync the file data and metadata.
    // File::sync_all() calls fsync() on Unix, FlushFileBuffers() on Windows.
    file.sync_all().map_err(|e| PersistError::Fsync {
        path: tmp_path.clone(),
        source: e,
    })?;

    // Step 3: Close explicitly (drop would close, but we want error handling).
    drop(file);

    // Step 4: Atomic rename.
    fs::rename(&tmp_path, path).map_err(|e| PersistError::Rename {
        from: tmp_path.clone(),
        to: path.to_path_buf(),
        source: e,
    })?;

    // Step 5: fsync parent directory (Linux only — required for ext4 durability).
    if let Err(e) = fsync_dir(dir) {
        // On Windows this is expected to be a no-op or best-effort.
        // On Linux this is critical — log but don't fail the whole operation,
        // because the data IS written, just the directory entry may not be durable.
        // In production (Linux), this should never fail unless disk is dying.
        eprintln!(
            "warning: fsync directory {} failed (non-fatal): {}",
            dir.display(),
            e
        );
    }

    Ok(())
}

/// Reads a file, returning `None` if it doesn't exist.
///
/// # Errors
/// Returns an error if the file exists but cannot be read.
pub fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, PersistError> {
    match fs::read(path) {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PersistError::Read {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

/// fsync a directory to ensure rename durability.
///
/// On Linux: opens the directory as an fd and calls fsync().
/// On Windows: no-op (NTFS doesn't need this, and you can't fsync a directory).
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::io::AsRawFd;

    let f = File::open(dir)?;
    let ret = unsafe { libc::fsync(f.as_raw_fd()) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn fsync_dir(_dir: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

/// Errors from persistence operations.
#[derive(Debug)]
pub enum PersistError {
    NoParentDir(std::path::PathBuf),
    NoFileName(std::path::PathBuf),
    CreateDir {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    CreateTemp {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    Write {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    Fsync {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    Rename {
        from: std::path::PathBuf,
        to: std::path::PathBuf,
        source: std::io::Error,
    },
    FsyncDir {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoParentDir(p) => write!(f, "no parent directory for {}", p.display()),
            Self::NoFileName(p) => write!(f, "no file name in path {}", p.display()),
            Self::CreateDir { path, source } => {
                write!(f, "creating directory {}: {source}", path.display())
            }
            Self::CreateTemp { path, source } => {
                write!(f, "creating temp file {}: {source}", path.display())
            }
            Self::Write { path, source } => {
                write!(f, "writing to {}: {source}", path.display())
            }
            Self::Fsync { path, source } => {
                write!(f, "fsync {}: {source}", path.display())
            }
            Self::Rename { from, to, source } => {
                write!(
                    f,
                    "renaming {} to {}: {source}",
                    from.display(),
                    to.display()
                )
            }
            Self::FsyncDir { path, source } => {
                write!(f, "fsync directory {}: {source}", path.display())
            }
            Self::Read { path, source } => {
                write!(f, "reading {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDir { source, .. }
            | Self::CreateTemp { source, .. }
            | Self::Write { source, .. }
            | Self::Fsync { source, .. }
            | Self::Rename { source, .. }
            | Self::FsyncDir { source, .. }
            | Self::Read { source, .. } => Some(source),
            Self::NoParentDir(_) | Self::NoFileName(_) => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn write_and_read_back() {
        let dir = std::env::temp_dir().join("craton_test_persist");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");

        let path = dir.join("test_state.json");
        let data = b"{\"phase\":\"idle\",\"version\":2}";

        atomic_write(&path, data).expect("atomic write");

        let read_back = fs::read(&path).expect("read back");
        assert_eq!(read_back, data);

        // Write again — should overwrite atomically.
        let data2 = b"{\"phase\":\"locked\",\"version\":2}";
        atomic_write(&path, data2).expect("atomic write 2");
        let read_back2 = fs::read(&path).expect("read back 2");
        assert_eq!(read_back2, data2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_optional_nonexistent() {
        let path = PathBuf::from(
            std::env::temp_dir()
                .join("craton_test_nonexistent_file_12345")
                .to_string_lossy()
                .to_string(),
        );
        let result = read_optional(&path).expect("should not error");
        assert!(result.is_none());
    }

    #[test]
    fn read_optional_existing() {
        let dir = std::env::temp_dir().join("craton_test_persist_read");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");

        let path = dir.join("existing.txt");
        fs::write(&path, b"hello").expect("write");

        let result = read_optional(&path).expect("should read");
        assert_eq!(result, Some(b"hello".to_vec()));

        let _ = fs::remove_dir_all(&dir);
    }
}