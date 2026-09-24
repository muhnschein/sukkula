//! S3: the only code in Sukkula that writes a received file.
//!
//! A file is written to a staging directory under a random name with
//! `O_CREAT|O_EXCL` and mode `0600`, never grows past the size its offer
//! declared, is hashed as it arrives and checked against the sender's digest
//! when there is one, and is only then placed in the target directory under
//! a name nobody else holds -- `photo.jpg`, else `photo (1).jpg`, and so on.
//! Placing never overwrites and never follows a link: it is `link(2)`, which
//! fails on any existing entry, or, where the file system cannot link, a copy
//! into a file opened `O_CREAT|O_EXCL`.
//!
//! An [`Incoming`] that is dropped without [`Incoming::commit`] deletes its
//! staging file, so a failure anywhere -- a peer hanging up, a cancel, a
//! digest mismatch, a panic in the adapter -- leaves nothing behind.
//!
//! The staging directory is a hidden directory *inside* the target directory
//! by default ([`Inbox::open`]): Sailjail's bind mounts make a link or rename
//! from the app's data directory into `~/Downloads` fail with `EXDEV`, and
//! staging next to the destination keeps placement a link rather than a copy.

use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

use crate::hex;
use crate::limits::MAX_FILE_BYTES;
use crate::name::SafeName;

/// The staging directory's name inside the target directory. A [`SafeName`]
/// never starts with a dot, so no received file can take this name.
pub const STAGING_DIR: &str = ".partial";

/// How many alternative names are tried before giving up.
const MAX_NAME_ATTEMPTS: u32 = 1000;

/// Space left free on the file system when checking an offer against it.
const SPACE_RESERVE: u64 = 64 * 1024 * 1024;

/// Most stale staging files removed when an inbox opens.
const MAX_STALE_SWEEP: usize = 10_000;

/// Why a write was refused or failed.
#[derive(Debug, Error)]
pub enum InboxError {
    /// A directory the inbox needs is a symlink, or not a directory.
    #[error("{0} is not a plain directory")]
    NotADirectory(PathBuf),
    /// The declared size is over [`MAX_FILE_BYTES`].
    #[error("the file is larger than the limit")]
    TooLarge,
    /// The sender sent more bytes than it declared.
    #[error("the sender sent more than the declared size")]
    Overflow,
    /// The sender stopped before the declared size.
    #[error("the file ended after {got} of {declared} bytes")]
    Truncated {
        /// Bytes received.
        got: u64,
        /// Bytes declared.
        declared: u64,
    },
    /// The content does not match the sender's digest.
    #[error("the file does not match its checksum")]
    DigestMismatch,
    /// Not enough free space for the offer.
    #[error("not enough free space")]
    NoSpace,
    /// Every candidate name was taken.
    #[error("no free file name")]
    NoFreeName,
    /// The file system said no.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
}

/// A file that has been received and placed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    /// The name it was saved under, which may be numbered.
    pub name: SafeName,
    /// Where it is.
    pub path: PathBuf,
    /// How many bytes it holds.
    pub size: u64,
}

/// A target directory and its staging directory.
#[derive(Clone, Debug)]
pub struct Inbox {
    target: PathBuf,
    staging: PathBuf,
}

impl Inbox {
    /// Opens `target` (creating it `0700` if needed) with its staging
    /// directory at `target/.partial`, and removes any staging files a
    /// previous run left behind.
    ///
    /// # Errors
    ///
    /// When either directory cannot be created, or exists as something other
    /// than a plain directory.
    pub fn open(target: &Path) -> Result<Inbox, InboxError> {
        Self::open_with_staging(target, &target.join(STAGING_DIR))
    }

    /// Like [`open`](Self::open), with the staging directory elsewhere.
    /// Placement falls back to copying when the two are on different mounts.
    ///
    /// # Errors
    ///
    /// As for [`open`](Self::open).
    pub fn open_with_staging(target: &Path, staging: &Path) -> Result<Inbox, InboxError> {
        ensure_private_dir(target)?;
        ensure_private_dir(staging)?;
        let inbox = Inbox {
            target: target.to_path_buf(),
            staging: staging.to_path_buf(),
        };
        inbox.sweep_stale();
        Ok(inbox)
    }

    /// Where received files end up.
    #[must_use]
    pub fn target_dir(&self) -> &Path {
        &self.target
    }

    /// Bytes available to an unprivileged writer on the target's file
    /// system, if the file system says.
    #[must_use]
    pub fn available_bytes(&self) -> Option<u64> {
        let st = rustix::fs::statvfs(&self.target).ok()?;
        st.f_bavail.checked_mul(st.f_frsize)
    }

    /// Refuses an offer of `total` bytes that would leave less than a small
    /// reserve free. Passes when the file system does not report its space.
    ///
    /// # Errors
    ///
    /// [`InboxError::NoSpace`].
    pub fn check_space(&self, total: u64) -> Result<(), InboxError> {
        match self.available_bytes() {
            Some(free) if total.saturating_add(SPACE_RESERVE) > free => Err(InboxError::NoSpace),
            _ => Ok(()),
        }
    }

    /// Starts receiving `name`, which the sender declared to be `size` bytes
    /// with the given digest.
    ///
    /// # Errors
    ///
    /// [`InboxError::TooLarge`] over the limit, or the staging file could
    /// not be created.
    pub async fn begin(
        &self,
        name: &SafeName,
        size: u64,
        sha256: Option<[u8; 32]>,
    ) -> Result<Incoming, InboxError> {
        if size > MAX_FILE_BYTES {
            return Err(InboxError::TooLarge);
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
        let staging_path = self.staging.join(format!("{}.part", hex::encode(&random)));
        let file = create_exclusive(&staging_path).await?;
        Ok(Incoming {
            file: Some(file),
            staging_path,
            target: self.target.clone(),
            name: name.clone(),
            declared: size,
            written: 0,
            hasher: Sha256::new(),
            expected: sha256,
            done: false,
        })
    }

    fn sweep_stale(&self) {
        let Ok(entries) = std::fs::read_dir(&self.staging) else {
            return;
        };
        for entry in entries.take(MAX_STALE_SWEEP).flatten() {
            let path = entry.path();
            let is_part = path.extension().is_some_and(|e| e == "part");
            let is_file = entry.file_type().is_ok_and(|t| t.is_file());
            if is_part && is_file {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// One file being received. Dropping it without [`commit`](Self::commit)
/// deletes what was written.
#[derive(Debug)]
pub struct Incoming {
    file: Option<tokio::fs::File>,
    staging_path: PathBuf,
    target: PathBuf,
    name: SafeName,
    declared: u64,
    written: u64,
    hasher: Sha256,
    expected: Option<[u8; 32]>,
    done: bool,
}

impl Incoming {
    /// Appends `chunk`.
    ///
    /// # Errors
    ///
    /// [`InboxError::Overflow`] if the chunk would take the file past its
    /// declared size -- nothing of that chunk is written -- or an I/O error.
    pub async fn write(&mut self, chunk: &[u8]) -> Result<(), InboxError> {
        let len = u64::try_from(chunk.len()).map_err(|_| InboxError::Overflow)?;
        let after = self.written.checked_add(len).ok_or(InboxError::Overflow)?;
        if after > self.declared {
            return Err(InboxError::Overflow);
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("file already closed"))?;
        file.write_all(chunk).await?;
        self.hasher.update(chunk);
        self.written = after;
        Ok(())
    }

    /// Bytes written so far.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Bytes declared.
    #[must_use]
    pub fn declared(&self) -> u64 {
        self.declared
    }

    /// The name it will be saved under, before numbering.
    #[must_use]
    pub fn name(&self) -> &SafeName {
        &self.name
    }

    /// Checks the size and digest, and places the file.
    ///
    /// # Errors
    ///
    /// [`InboxError::Truncated`], [`InboxError::DigestMismatch`],
    /// [`InboxError::NoFreeName`] or an I/O error. The staging file is gone
    /// either way.
    pub async fn commit(mut self) -> Result<Saved, InboxError> {
        if self.written != self.declared {
            return Err(InboxError::Truncated {
                got: self.written,
                declared: self.declared,
            });
        }
        if let Some(mut file) = self.file.take() {
            file.flush().await?;
            file.sync_all().await?;
        }
        if let Some(expected) = self.expected {
            let actual: [u8; 32] = std::mem::take(&mut self.hasher).finalize().into();
            if actual != expected {
                return Err(InboxError::DigestMismatch);
            }
        }
        ensure_plain_dir(&self.target)?;
        for n in 0..MAX_NAME_ATTEMPTS {
            let candidate = self.name.numbered(n);
            let dest = self.target.join(candidate.as_str());
            match place(&self.staging_path, &dest).await {
                Ok(()) => {
                    let _ = tokio::fs::remove_file(&self.staging_path).await;
                    self.done = true;
                    return Ok(Saved {
                        name: candidate,
                        path: dest,
                        size: self.declared,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(InboxError::NoFreeName)
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        if !self.done {
            self.file.take();
            let _ = std::fs::remove_file(&self.staging_path);
        }
    }
}

/// Puts the staged file at `dest`, which must not exist.
async fn place(staged: &Path, dest: &Path) -> io::Result<()> {
    match tokio::fs::hard_link(staged, dest).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        // EXDEV across mounts, EPERM on file systems without links: copy
        // into an exclusively created file instead.
        Err(_) => {
            let mut out = create_exclusive(dest).await?;
            let copied = async {
                let mut src = tokio::fs::File::open(staged).await?;
                tokio::io::copy(&mut src, &mut out).await?;
                out.flush().await?;
                out.sync_all().await
            }
            .await;
            if let Err(e) = copied {
                drop(out);
                let _ = tokio::fs::remove_file(dest).await;
                return Err(e);
            }
            Ok(())
        }
    }
}

#[allow(clippy::disallowed_methods)] // S3: this is the inbox.
async fn create_exclusive(path: &Path) -> io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .await
}

/// Creates `dir` `0700` if missing, and checks it is a plain directory
/// owned by us.
pub(crate) fn ensure_private_dir(dir: &Path) -> Result<(), InboxError> {
    match std::fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        Err(e) => return Err(e.into()),
    }
    ensure_plain_dir(dir)
}

fn ensure_plain_dir(dir: &Path) -> Result<(), InboxError> {
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(InboxError::NotADirectory(dir.to_path_buf()));
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(InboxError::NotADirectory(dir.to_path_buf()));
    }
    // Group- or world-writable would let another user swap entries under us.
    if meta.permissions().mode() & 0o022 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Tests set the scene with plain writes.
mod tests {
    use super::*;
    use crate::name::sanitize;
    use std::os::unix::fs::PermissionsExt;

    fn staged_files(dir: &Path) -> usize {
        std::fs::read_dir(dir.join(STAGING_DIR)).unwrap().count()
    }

    #[tokio::test]
    async fn a_file_is_received_and_placed() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        let mut f = inbox.begin(&sanitize("a.txt"), 5, None).await.unwrap();
        f.write(b"hel").await.unwrap();
        f.write(b"lo").await.unwrap();
        let saved = f.commit().await.unwrap();
        assert_eq!(saved.name.as_str(), "a.txt");
        assert_eq!(std::fs::read(&saved.path).unwrap(), b"hello");
        let mode = std::fs::metadata(&saved.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(staged_files(dir.path()), 0);
    }

    #[tokio::test]
    async fn existing_files_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"mine").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("a (1).txt")).unwrap();
        let mut f = inbox.begin(&sanitize("a.txt"), 3, None).await.unwrap();
        f.write(b"new").await.unwrap();
        let saved = f.commit().await.unwrap();
        assert_eq!(saved.name.as_str(), "a (2).txt");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"mine");
        assert!(
            std::fs::symlink_metadata(dir.path().join("a (1).txt"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn overflow_is_refused_and_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        let mut f = inbox.begin(&sanitize("a"), 3, None).await.unwrap();
        assert!(matches!(
            f.write(b"toolong").await,
            Err(InboxError::Overflow)
        ));
        assert_eq!(f.written(), 0);
        drop(f);
        assert_eq!(staged_files(dir.path()), 0);
        assert!(!dir.path().join("a").exists());
    }

    #[tokio::test]
    async fn short_files_are_refused_and_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        let mut f = inbox.begin(&sanitize("a"), 10, None).await.unwrap();
        f.write(b"abc").await.unwrap();
        assert!(matches!(
            f.commit().await,
            Err(InboxError::Truncated {
                got: 3,
                declared: 10
            })
        ));
        assert_eq!(staged_files(dir.path()), 0);
        assert!(!dir.path().join("a").exists());
    }

    #[tokio::test]
    async fn digests_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        let good: [u8; 32] = Sha256::digest(b"abc").into();
        let mut f = inbox.begin(&sanitize("ok"), 3, Some(good)).await.unwrap();
        f.write(b"abc").await.unwrap();
        assert!(f.commit().await.is_ok());
        let mut f = inbox.begin(&sanitize("bad"), 3, Some(good)).await.unwrap();
        f.write(b"abd").await.unwrap();
        assert!(matches!(f.commit().await, Err(InboxError::DigestMismatch)));
        assert!(!dir.path().join("bad").exists());
        assert_eq!(staged_files(dir.path()), 0);
    }

    #[tokio::test]
    async fn oversized_declarations_are_refused_up_front() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        assert!(matches!(
            inbox.begin(&sanitize("a"), MAX_FILE_BYTES + 1, None).await,
            Err(InboxError::TooLarge)
        ));
        assert!(matches!(
            inbox.check_space(u64::MAX),
            Err(InboxError::NoSpace)
        ));
    }

    #[tokio::test]
    async fn stale_staging_files_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        drop(Inbox::open(dir.path()).unwrap());
        std::fs::write(dir.path().join(STAGING_DIR).join("dead.part"), b"x").unwrap();
        drop(Inbox::open(dir.path()).unwrap());
        assert_eq!(staged_files(dir.path()), 0);
    }

    #[test]
    fn a_symlinked_target_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(matches!(
            Inbox::open(&link),
            Err(InboxError::NotADirectory(_))
        ));
    }

    #[tokio::test]
    async fn placement_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::write(&src, b"data").unwrap();
        let dest = dir.path().join("dest");
        place(&src, &dest).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"data");
        assert_eq!(
            place(&src, &dest).await.unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }
}
