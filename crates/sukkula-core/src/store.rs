//! The app's own small files: settings, the LocalSend certificate and key.
//!
//! S3 and S9. Everything is `0600` in a `0700` directory, written to a
//! temporary file and renamed into place so a crash leaves the old version,
//! read with a size cap, and never through a symlink. A secret that is
//! group- or world-readable, or not owned by us, is refused rather than
//! used: something other than this code wrote it.

use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::hex;
use crate::inbox::{InboxError, ensure_private_dir};

/// Why a store operation failed.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The store directory is not a plain directory of ours.
    #[error("the data directory is unusable: {0}")]
    Directory(String),
    /// A file name that is not a plain lowercase name.
    #[error("invalid store file name")]
    BadName,
    /// The file is a symlink, not a regular file, or not ours.
    #[error("{0} is not a regular file owned by this user")]
    NotRegular(String),
    /// A secret readable by others.
    #[error("{0} is readable by other users")]
    Exposed(String),
    /// Larger than the caller's cap.
    #[error("{0} is larger than expected")]
    TooLarge(String),
    /// The JSON did not parse.
    #[error("{0} is malformed: {1}")]
    Malformed(String, String),
    /// The file system said no.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
}

impl From<InboxError> for StoreError {
    fn from(e: InboxError) -> Self {
        StoreError::Directory(e.to_string())
    }
}

/// The app data directory.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Opens `dir`, creating it `0700` if needed.
    ///
    /// # Errors
    ///
    /// When it cannot be created or is not a plain directory of ours.
    pub fn open(dir: &Path) -> Result<Store, StoreError> {
        ensure_private_dir(dir)?;
        Ok(Store {
            dir: dir.to_path_buf(),
        })
    }

    /// The directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads `name`, at most `max` bytes. `Ok(None)` if it does not exist.
    ///
    /// # Errors
    ///
    /// A symlink, a non-regular file, a file not ours, a file over `max`.
    pub fn read(&self, name: &str, max: usize) -> Result<Option<Vec<u8>>, StoreError> {
        self.read_checked(name, max, false)
    }

    /// Like [`read`](Self::read), and also refuses a file any other user
    /// can read.
    ///
    /// # Errors
    ///
    /// As for [`read`](Self::read), and [`StoreError::Exposed`].
    pub fn read_secret(&self, name: &str, max: usize) -> Result<Option<Vec<u8>>, StoreError> {
        self.read_checked(name, max, true)
    }

    /// Replaces `name` with `bytes`, atomically, mode `0600`.
    ///
    /// # Errors
    ///
    /// An I/O error; the old contents are intact.
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let path = self.path(name)?;
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
        let tmp = self
            .dir
            .join(format!(".{name}.{}.tmp", hex::encode(&random)));
        let result = (|| {
            let mut f = open_exclusive(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            rename(&tmp, &path)?;
            // The rename is durable once the directory is.
            std::fs::File::open(&self.dir)?.sync_all()
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(result?)
    }

    /// Reads and parses a JSON file, at most `max` bytes.
    ///
    /// # Errors
    ///
    /// As for [`read`](Self::read), and [`StoreError::Malformed`].
    pub fn read_json<T: DeserializeOwned>(
        &self,
        name: &str,
        max: usize,
    ) -> Result<Option<T>, StoreError> {
        match self.read(name, max)? {
            None => Ok(None),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| StoreError::Malformed(name.to_owned(), e.to_string())),
        }
    }

    /// Serialises `value` as JSON and [`write`](Self::write)s it.
    ///
    /// # Errors
    ///
    /// As for [`write`](Self::write).
    pub fn write_json<T: Serialize>(&self, name: &str, value: &T) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|e| StoreError::Malformed(name.to_owned(), e.to_string()))?;
        self.write(name, &bytes)
    }

    fn read_checked(
        &self,
        name: &str,
        max: usize,
        secret: bool,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.path(name)?;
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if !meta.file_type().is_file() || meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(StoreError::NotRegular(name.to_owned()));
        }
        if secret && meta.mode() & 0o077 != 0 {
            return Err(StoreError::Exposed(name.to_owned()));
        }
        let cap = u64::try_from(max).unwrap_or(u64::MAX);
        if meta.len() > cap {
            return Err(StoreError::TooLarge(name.to_owned()));
        }
        let file = open_read_nofollow(&path)?;
        let mut out = Vec::new();
        file.take(cap.saturating_add(1)).read_to_end(&mut out)?;
        if out.len() > max {
            return Err(StoreError::TooLarge(name.to_owned()));
        }
        Ok(Some(out))
    }

    fn path(&self, name: &str) -> Result<PathBuf, StoreError> {
        let ok = !name.is_empty()
            && name.len() <= 64
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
        if !ok {
            return Err(StoreError::BadName);
        }
        Ok(self.dir.join(name))
    }
}

fn nofollow() -> i32 {
    i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).unwrap_or(0)
}

#[allow(clippy::disallowed_methods)] // S3: this is the store.
fn open_exclusive(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nofollow())
        .open(path)
}

#[allow(clippy::disallowed_methods)] // Reading, with O_NOFOLLOW.
fn open_read_nofollow(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nofollow())
        .open(path)
}

#[allow(clippy::disallowed_methods)] // S3: this is the store.
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Tests set the scene with plain writes.
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn round_trip_with_private_modes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("data")).unwrap();
        store.write("key.pem", b"secret").unwrap();
        assert_eq!(
            store.read_secret("key.pem", 100).unwrap().unwrap(),
            b"secret"
        );
        let mode = std::fs::metadata(store.dir().join("key.pem"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let dmode = std::fs::metadata(store.dir()).unwrap().permissions().mode();
        assert_eq!(dmode & 0o777, 0o700);
        store.write("key.pem", b"other").unwrap();
        assert_eq!(store.read("key.pem", 100).unwrap().unwrap(), b"other");
        assert_eq!(store.read("missing", 100).unwrap(), None);
    }

    #[test]
    fn exposed_secrets_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.write("key.pem", b"secret").unwrap();
        std::fs::set_permissions(
            dir.path().join("key.pem"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(matches!(
            store.read_secret("key.pem", 100),
            Err(StoreError::Exposed(_))
        ));
    }

    #[test]
    fn symlinks_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", dir.path().join("settings.json")).unwrap();
        assert!(matches!(
            store.read("settings.json", 100),
            Err(StoreError::NotRegular(_))
        ));
    }

    #[test]
    fn oversized_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.write("big", &[0u8; 101]).unwrap();
        assert!(matches!(
            store.read("big", 100),
            Err(StoreError::TooLarge(_))
        ));
    }

    #[test]
    fn names_are_plain() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        for bad in ["", "../x", "a/b", ".hidden", "UPPER", "a b"] {
            assert!(
                matches!(store.write(bad, b"x"), Err(StoreError::BadName)),
                "{bad}"
            );
        }
    }

    #[test]
    fn malformed_json_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.write("s.json", b"{nope").unwrap();
        assert!(matches!(
            store.read_json::<serde_json::Value>("s.json", 100),
            Err(StoreError::Malformed(..))
        ));
    }
}
