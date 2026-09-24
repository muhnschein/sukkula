//! The app's own small files: settings, the LocalSend certificate and key.
//!
//! S3 and S9. Everything is `0600` in a `0700` directory, written to a
//! temporary file and renamed into place so a crash leaves the old version,
//! read with a size cap, and never through a symlink. A secret that is
//! group- or world-readable, or not owned by us, is refused rather than
//! used: something other than this code wrote it.
//!
//! Every operation opens the directory by descriptor (the private `dirfd` module) and
//! works relative to it, and every read checks the file it actually opened,
//! not the name it asked for.

use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::dirfd::{self, DirError, Share};
use crate::hex;
use crate::inbox::InboxError;

/// Longest store file name.
const MAX_STORE_NAME: usize = 64;

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
    /// Opens `dir`, creating it `0700` if needed, and tightening it to
    /// `0700` if it is looser (S9).
    ///
    /// # Errors
    ///
    /// When it cannot be created or is not a plain directory of ours.
    pub fn open(dir: &Path) -> Result<Store, StoreError> {
        let store = Store {
            dir: dir.to_path_buf(),
        };
        store.open_dir()?;
        Ok(store)
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
    // S3: the store's writer; removes its own temporary file on failure.
    #[allow(clippy::disallowed_methods)]
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        check_name(name)?;
        let dir = self.open_dir()?;
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
        // A leading dot: no store name has one, so a temporary file can
        // never be mistaken for, or collide with, a real one.
        let tmp = format!(".{name}.{}.tmp", hex::encode(&random));
        let result = replace(&dir, &tmp, name, bytes);
        if result.is_err() {
            let _ = rustix::fs::unlinkat(&dir, tmp.as_str(), AtFlags::empty());
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

    // S3: an O_RDONLY|O_NOFOLLOW open relative to the checked directory.
    #[allow(clippy::disallowed_methods)]
    fn read_checked(
        &self,
        name: &str,
        max: usize,
        secret: bool,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        check_name(name)?;
        let dir = self.open_dir()?;
        // Open first, then check what was opened: checking the name and
        // then opening it would let the entry change in between. O_NONBLOCK
        // so that a FIFO planted here cannot hang the open, O_NOCTTY so that
        // a terminal cannot become ours.
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        let fd = match rustix::fs::openat(&dir, name, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            // A symlink (O_NOFOLLOW), or a socket.
            Err(Errno::LOOP | Errno::NXIO) => {
                return Err(StoreError::NotRegular(name.to_owned()));
            }
            Err(e) => return Err(StoreError::Io(e.into())),
        };
        let st = rustix::fs::fstat(&fd).map_err(io::Error::from)?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
            || st.st_uid != rustix::process::geteuid().as_raw()
        {
            return Err(StoreError::NotRegular(name.to_owned()));
        }
        if secret && st.st_mode & 0o077 != 0 {
            return Err(StoreError::Exposed(name.to_owned()));
        }
        let cap = u64::try_from(max).unwrap_or(u64::MAX);
        // S4: the size is checked before anything is allocated for it...
        if u64::try_from(st.st_size).map_or(true, |len| len > cap) {
            return Err(StoreError::TooLarge(name.to_owned()));
        }
        // ...and the read is capped too, for a file that grows meanwhile.
        let mut out = Vec::new();
        std::fs::File::from(fd)
            .take(cap.saturating_add(1))
            .read_to_end(&mut out)?;
        if out.len() > max {
            return Err(StoreError::TooLarge(name.to_owned()));
        }
        Ok(Some(out))
    }

    fn open_dir(&self) -> Result<OwnedFd, StoreError> {
        dirfd::open(&self.dir, Share::Nothing).map_err(|e| match e {
            DirError::NotPlain => StoreError::Directory(format!(
                "{} is not a plain directory owned by this user",
                self.dir.display()
            )),
            DirError::Io(e) => StoreError::Directory(e.to_string()),
        })
    }
}

/// Writes `bytes` to the new file `tmp` in `dir`, makes it durable, and
/// renames it over `name`.
// S3: the store's one write: O_CREAT|O_EXCL 0600, then renameat.
#[allow(clippy::disallowed_methods)]
fn replace(dir: &OwnedFd, tmp: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
    let fd = rustix::fs::openat(
        dir,
        tmp,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?;
    let mut f = std::fs::File::from(fd);
    f.write_all(bytes)?;
    f.sync_all()?;
    // rename(2) replaces the entry itself, a symlink included; it never
    // writes through one.
    rustix::fs::renameat(dir, tmp, dir, name)?;
    // The rename is durable once the directory is.
    rustix::fs::fsync(dir)?;
    Ok(())
}

/// A store name is one plain lowercase component: no separators, no leading
/// dot (so never `.`, `..` or a temporary file), at most [`MAX_STORE_NAME`]
/// bytes.
fn check_name(name: &str) -> Result<(), StoreError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_STORE_NAME
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if ok { Ok(()) } else { Err(StoreError::BadName) }
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
        // Nothing but the file itself is left behind.
        assert_eq!(std::fs::read_dir(store.dir()).unwrap().count(), 1);
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
        // Not a secret: readable.
        assert_eq!(store.read("key.pem", 100).unwrap().unwrap(), b"secret");
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
        // A write replaces the link itself and never writes through it.
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"untouched").unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join("key.pem")).unwrap();
        store.write("key.pem", b"new").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"untouched");
        assert!(
            !std::fs::symlink_metadata(dir.path().join("key.pem"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn a_symlinked_store_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(matches!(Store::open(&link), Err(StoreError::Directory(_))));
        // And one swapped in after opening is refused at the next use.
        let data = dir.path().join("data");
        let store = Store::open(&data).unwrap();
        std::fs::remove_dir(&data).unwrap();
        std::os::unix::fs::symlink(&real, &data).unwrap();
        assert!(matches!(
            store.write("settings.json", b"{}"),
            Err(StoreError::Directory(_))
        ));
        assert!(matches!(
            store.read("settings.json", 100),
            Err(StoreError::Directory(_))
        ));
        assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
    }

    #[test]
    fn a_loose_store_directory_is_tightened() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        Store::open(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn special_files_are_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        // A FIFO: a blocking open would wait for a writer forever.
        rustix::fs::mknodat(
            rustix::fs::CWD,
            dir.path().join("fifo"),
            FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        assert!(matches!(
            store.read("fifo", 100),
            Err(StoreError::NotRegular(_))
        ));
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        assert!(matches!(
            store.read("subdir", 100),
            Err(StoreError::NotRegular(_))
        ));
        // A directory cannot be replaced by a write; nothing is left over.
        assert!(store.write("subdir", b"x").is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn a_file_owned_by_someone_else_is_refused() {
        if !rustix::process::geteuid().is_root() {
            eprintln!("skipped: not root, cannot chown");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.write("key.pem", b"planted").unwrap();
        rustix::fs::chown(
            dir.path().join("key.pem"),
            Some(rustix::fs::Uid::from_raw(4242)),
            None,
        )
        .unwrap();
        assert!(matches!(
            store.read_secret("key.pem", 100),
            Err(StoreError::NotRegular(_))
        ));
        assert!(matches!(
            store.read("key.pem", 100),
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
        assert_eq!(store.read("big", 101).unwrap().unwrap().len(), 101);
        assert!(matches!(store.read("big", 0), Err(StoreError::TooLarge(_))));
    }

    #[test]
    fn names_are_plain() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let long = "a".repeat(MAX_STORE_NAME + 1);
        for bad in [
            "", "../x", "a/b", ".hidden", "UPPER", "a b", "..", ".", &long,
        ] {
            assert!(
                matches!(store.write(bad, b"x"), Err(StoreError::BadName)),
                "{bad}"
            );
            assert!(
                matches!(store.read(bad, 10), Err(StoreError::BadName)),
                "{bad}"
            );
        }
        store.write(&"a".repeat(MAX_STORE_NAME), b"x").unwrap();
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
        store
            .write_json("t.json", &serde_json::json!({"a": 1}))
            .unwrap();
        assert_eq!(
            store
                .read_json::<serde_json::Value>("t.json", 100)
                .unwrap()
                .unwrap(),
            serde_json::json!({"a": 1})
        );
        assert_eq!(
            store
                .read_json::<serde_json::Value>("none.json", 100)
                .unwrap(),
            None
        );
    }

    #[test]
    fn errors_describe_themselves() {
        let e = StoreError::from(InboxError::NoSpace);
        assert!(matches!(e, StoreError::Directory(_)));
        for e in [
            StoreError::Directory("d".into()),
            StoreError::BadName,
            StoreError::NotRegular("n".into()),
            StoreError::Exposed("n".into()),
            StoreError::TooLarge("n".into()),
            StoreError::Malformed("n".into(), "m".into()),
            StoreError::Io(io::Error::other("x")),
        ] {
            assert!(!e.to_string().is_empty());
        }
    }
}
