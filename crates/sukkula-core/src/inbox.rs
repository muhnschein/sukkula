//! S3: the only code in Sukkula that writes a received file.
//!
//! A file is written to a staging directory under a random name with
//! `O_CREAT|O_EXCL|O_NOFOLLOW` and mode `0600`, never grows past the size its
//! offer declared, is hashed as it arrives and checked against the sender's
//! digest when there is one, and is only then placed in the target directory
//! under a name nobody else holds -- `photo.jpg`, else `photo (1).jpg`, and so
//! on. Placing never overwrites and never follows a link: it is `linkat(2)`,
//! which fails on any existing entry, or, where the file system cannot link
//! (`EXDEV` across mounts, `EPERM` on FAT), a copy from the staging file's own
//! descriptor into a file opened `O_CREAT|O_EXCL|O_NOFOLLOW`.
//!
//! Both directories are held open by descriptor from [`Inbox::begin`] to the
//! end of the file (the private `dirfd` module), and every operation is relative to
//! them, so swapping `.partial` or the target for a symlink mid-transfer
//! redirects nothing. After a link, the new entry is checked to be the very
//! inode that was written; anything else is removed again.
//!
//! An [`Incoming`] that is dropped without [`Incoming::commit`] deletes its
//! staging file, so a failure anywhere -- a peer hanging up, a cancel, a
//! digest mismatch, a panic in the adapter, the task being aborted -- leaves
//! nothing behind. A commit cancelled half-way through the copy fallback
//! removes the half-copied destination too.
//!
//! The staging directory is a hidden directory *inside* the target directory
//! by default ([`Inbox::open`]): Sailjail's bind mounts make a link or rename
//! from the app's data directory into `~/Downloads` fail with `EXDEV`, and
//! staging next to the destination keeps placement a link rather than a copy.

use std::io::{self, SeekFrom};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::dirfd::{self, DirError, FileId, Share};
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

/// The suffix of a staging file.
const PART_SUFFIX: &str = ".part";

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
    /// The file system said no. Also what a file that failed earlier, or
    /// whose staging copy was tampered with, reports from then on.
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
    staging: Staging,
}

#[derive(Clone, Debug)]
enum Staging {
    /// [`STAGING_DIR`] inside the target, opened relative to the target's
    /// descriptor so that no path is resolved twice.
    Nested,
    /// A directory of its own.
    At(PathBuf),
}

/// Both directories, open.
#[derive(Debug)]
struct Dirs {
    target: OwnedFd,
    staging: OwnedFd,
}

impl Inbox {
    /// Opens `target` (creating it `0700` if needed) with its staging
    /// directory at `target/.partial`, and removes any staging files a
    /// previous run left behind.
    ///
    /// # Errors
    ///
    /// When either directory cannot be created, or exists as something other
    /// than a plain directory owned by this user.
    pub fn open(target: &Path) -> Result<Inbox, InboxError> {
        Self::with(target.to_path_buf(), Staging::Nested)
    }

    /// Like [`open`](Self::open), with the staging directory elsewhere.
    /// Placement falls back to copying when the two are on different mounts.
    ///
    /// # Errors
    ///
    /// As for [`open`](Self::open).
    pub fn open_with_staging(target: &Path, staging: &Path) -> Result<Inbox, InboxError> {
        Self::with(target.to_path_buf(), Staging::At(staging.to_path_buf()))
    }

    fn with(target: PathBuf, staging: Staging) -> Result<Inbox, InboxError> {
        let inbox = Inbox { target, staging };
        let dirs = inbox.open_dirs()?;
        sweep_stale(&dirs.staging);
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
    /// The directories are checked (and recreated, if the user deleted them
    /// since the inbox opened) here, and held open until the file is done.
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
        let dirs = self.open_dirs()?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
        let staging_name = format!("{}{PART_SUFFIX}", hex::encode(&random));
        // Read-write: the copy fallback reads the data back through this
        // same descriptor rather than reopening a name.
        let fd = create_exclusive(&dirs.staging, &staging_name, OFlags::RDWR)?;
        let id = dirfd::file_id(&rustix::fs::fstat(&fd).map_err(io::Error::from)?);
        Ok(Incoming {
            file: Some(tokio::fs::File::from_std(std::fs::File::from(fd))),
            dirs,
            staging_name,
            id,
            target: self.target.clone(),
            name: name.clone(),
            declared: size,
            written: 0,
            hasher: Sha256::new(),
            expected: sha256,
            failed: false,
            done: false,
        })
    }

    fn open_dirs(&self) -> Result<Dirs, InboxError> {
        let not_plain = |path: PathBuf| {
            move |e: DirError| match e {
                DirError::NotPlain => InboxError::NotADirectory(path),
                DirError::Io(e) => InboxError::Io(e),
            }
        };
        let target =
            dirfd::open(&self.target, Share::ReadOnly).map_err(not_plain(self.target.clone()))?;
        let staging = match &self.staging {
            Staging::Nested => dirfd::open_at(&target, STAGING_DIR, Share::Nothing)
                .map_err(not_plain(self.target.join(STAGING_DIR)))?,
            Staging::At(path) => {
                dirfd::open(path, Share::Nothing).map_err(not_plain(path.clone()))?
            }
        };
        Ok(Dirs { target, staging })
    }
}

/// Removes regular `*.part` files from the staging directory: what a
/// previous run left when it was killed mid-file.
fn sweep_stale(staging: &OwnedFd) {
    let Ok(dir) = rustix::fs::Dir::read_from(staging) else {
        return;
    };
    for entry in dir.take(MAX_STALE_SWEEP).flatten() {
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if !name.ends_with(PART_SUFFIX) {
            continue;
        }
        let regular = match entry.file_type() {
            FileType::RegularFile => true,
            // Some file systems do not fill in `d_type`.
            FileType::Unknown => rustix::fs::statat(staging, name, AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|st| FileType::from_raw_mode(st.st_mode) == FileType::RegularFile),
            _ => false,
        };
        if regular {
            let _ = rustix::fs::unlinkat(staging, name, AtFlags::empty());
        }
    }
}

/// One file being received. Dropping it without [`commit`](Self::commit)
/// deletes what was written.
#[derive(Debug)]
pub struct Incoming {
    file: Option<tokio::fs::File>,
    dirs: Dirs,
    staging_name: String,
    /// The staging file's inode, to recognise it by.
    id: FileId,
    target: PathBuf,
    name: SafeName,
    declared: u64,
    written: u64,
    hasher: Sha256,
    expected: Option<[u8; 32]>,
    /// A write failed, was cancelled half-way, or overflowed: what is on disk
    /// no longer matches the count, so nothing more is written and the file
    /// is never placed.
    failed: bool,
    /// Placed; the staging file is no longer ours to delete.
    done: bool,
}

impl Incoming {
    /// Appends `chunk`.
    ///
    /// # Errors
    ///
    /// [`InboxError::Overflow`] if the chunk would take the file past its
    /// declared size -- nothing of that chunk is written -- or an I/O error.
    /// Either way the file is spoiled: every later write and the commit fail
    /// too, and dropping it removes it.
    pub async fn write(&mut self, chunk: &[u8]) -> Result<(), InboxError> {
        if self.failed {
            return Err(spoiled());
        }
        let after = u64::try_from(chunk.len())
            .ok()
            .and_then(|len| self.written.checked_add(len))
            .filter(|after| *after <= self.declared);
        let Some(after) = after else {
            // A peer that sends more than it declared is not trusted with
            // the rest of the file either.
            self.failed = true;
            return Err(InboxError::Overflow);
        };
        let Some(file) = self.file.as_mut() else {
            return Err(spoiled());
        };
        // Spoiled until the write completes: a failed or cancelled
        // `write_all` may have put part of the chunk on disk, and the count
        // would no longer say how much.
        self.failed = true;
        file.write_all(chunk).await?;
        self.failed = false;
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
    /// either way, and so is anything this call put in the target directory.
    pub async fn commit(mut self) -> Result<Saved, InboxError> {
        if self.failed {
            return Err(spoiled());
        }
        if self.written != self.declared {
            return Err(InboxError::Truncated {
                got: self.written,
                declared: self.declared,
            });
        }
        let Some(file) = self.file.as_mut() else {
            return Err(spoiled());
        };
        file.flush().await?;
        file.sync_all().await?;
        // The count says `declared`; the file system must agree, or
        // something other than `write` has touched the staging file.
        let st = rustix::fs::fstat(file.as_fd()).map_err(io::Error::from)?;
        if u64::try_from(st.st_size).ok() != Some(self.declared) {
            return Err(tampered());
        }
        if let Some(expected) = self.expected {
            let actual: [u8; 32] = std::mem::take(&mut self.hasher).finalize().into();
            if actual != expected {
                return Err(InboxError::DigestMismatch);
            }
        }
        for n in 0..MAX_NAME_ATTEMPTS {
            let candidate = self.name.numbered(n);
            match self.place(candidate.as_str()).await {
                Ok(()) => {
                    // No await between placing and this, so a cancel cannot
                    // land in between and have `Drop` treat a placed file
                    // as a failed one.
                    self.done = true;
                    let _ = rustix::fs::unlinkat(
                        &self.dirs.staging,
                        self.staging_name.as_str(),
                        AtFlags::empty(),
                    );
                    return Ok(Saved {
                        path: self.target.join(candidate.as_str()),
                        name: candidate,
                        size: self.declared,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(InboxError::NoFreeName)
    }

    /// Puts the staged file at `dest` in the target directory, which must
    /// not exist.
    async fn place(&mut self, dest: &str) -> io::Result<()> {
        let staged = rustix::fs::statat(
            &self.dirs.staging,
            self.staging_name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )?;
        if dirfd::file_id(&staged) != self.id {
            return Err(tampered_io());
        }
        match rustix::fs::linkat(
            &self.dirs.staging,
            self.staging_name.as_str(),
            &self.dirs.target,
            dest,
            // No AT_SYMLINK_FOLLOW: a symlink swapped in since the check
            // above is linked as a symlink, and caught below.
            AtFlags::empty(),
        ) {
            Ok(()) => {
                if entry_id(self.dirs.target.as_fd(), dest) == Some(self.id) {
                    Ok(())
                } else {
                    // The staging entry was replaced between the check and
                    // the link. What was linked is not ours; take it away.
                    let _ = rustix::fs::unlinkat(&self.dirs.target, dest, AtFlags::empty());
                    Err(tampered_io())
                }
            }
            Err(e) if copy_instead(e) => self.copy_to(dest).await,
            Err(e) => Err(e.into()),
        }
    }

    /// The fallback where the file system cannot link: a copy, from the
    /// staging file's own descriptor, into a new exclusive file.
    async fn copy_to(&mut self, dest: &str) -> io::Result<()> {
        let out = create_exclusive(&self.dirs.target, dest, OFlags::WRONLY)?;
        let guard = Placing {
            dir: self.dirs.target.as_fd(),
            name: dest,
            id: dirfd::file_id(&rustix::fs::fstat(&out)?),
            armed: true,
        };
        let mut out = tokio::fs::File::from_std(std::fs::File::from(out));
        let src = self.file.as_mut().ok_or_else(spoiled_io)?;
        src.seek(SeekFrom::Start(0)).await?;
        // `take`: the copy is capped at the declared size as the write was.
        let copied = tokio::io::copy(&mut src.take(self.declared), &mut out).await?;
        if copied != self.declared {
            return Err(tampered_io());
        }
        out.flush().await?;
        out.sync_all().await?;
        guard.disarm();
        Ok(())
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        if !self.done {
            self.file.take();
            let _ = rustix::fs::unlinkat(
                &self.dirs.staging,
                self.staging_name.as_str(),
                AtFlags::empty(),
            );
        }
    }
}

/// A destination being copied into. Dropped armed -- the copy failed, or
/// the commit was cancelled mid-copy -- it removes the destination, but only
/// if the entry is still the file it created.
struct Placing<'a> {
    dir: BorrowedFd<'a>,
    name: &'a str,
    id: FileId,
    armed: bool,
}

impl Placing<'_> {
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for Placing<'_> {
    fn drop(&mut self) {
        if self.armed && entry_id(self.dir, self.name) == Some(self.id) {
            let _ = rustix::fs::unlinkat(self.dir, self.name, AtFlags::empty());
        }
    }
}

/// The identity of the entry `name` in `dir`, without following a symlink.
fn entry_id(dir: BorrowedFd<'_>, name: &str) -> Option<FileId> {
    rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
        .ok()
        .map(|st| dirfd::file_id(&st))
}

/// Whether a failed `linkat` means "this file system cannot link here" --
/// across mounts, or no hard links at all -- rather than a real error.
fn copy_instead(e: Errno) -> bool {
    matches!(
        e,
        Errno::XDEV | Errno::PERM | Errno::OPNOTSUPP | Errno::NOSYS | Errno::MLINK
    )
}

/// Creates `name` in `dir` with `O_CREAT|O_EXCL|O_NOFOLLOW`, mode `0600`.
/// `O_EXCL` alone already refuses an existing symlink; `O_NOFOLLOW` says so.
fn create_exclusive(dir: &OwnedFd, name: &str, access: OFlags) -> io::Result<OwnedFd> {
    let flags = access | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(rustix::fs::openat(
        dir,
        name,
        flags,
        Mode::from_raw_mode(0o600),
    )?)
}

fn spoiled_io() -> io::Error {
    io::Error::other("an earlier write to this file failed")
}

fn spoiled() -> InboxError {
    InboxError::Io(spoiled_io())
}

fn tampered_io() -> io::Error {
    io::Error::other("the staging file was changed by something else")
}

fn tampered() -> InboxError {
    InboxError::Io(tampered_io())
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Tests set the scene with plain writes.
mod tests {
    use super::*;
    use crate::name::sanitize;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::task::{Context, Poll, Waker};

    fn staged_files(dir: &Path) -> usize {
        std::fs::read_dir(dir.join(STAGING_DIR)).unwrap().count()
    }

    /// Entries in `dir`, other than the staging directory.
    fn placed(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n != STAGING_DIR)
            .collect();
        v.sort();
        v
    }

    /// The only staging file's name.
    fn the_staging_file(dir: &Path) -> PathBuf {
        let mut it = std::fs::read_dir(dir).unwrap();
        let p = it.next().unwrap().unwrap().path();
        assert!(it.next().is_none());
        p
    }

    #[tokio::test]
    async fn a_file_is_received_and_placed() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        assert_eq!(inbox.target_dir(), dir.path());
        let mut f = inbox.begin(&sanitize("a.txt"), 5, None).await.unwrap();
        assert_eq!(f.name().as_str(), "a.txt");
        assert_eq!(f.declared(), 5);
        f.write(b"hel").await.unwrap();
        f.write(b"lo").await.unwrap();
        assert_eq!(f.written(), 5);
        let saved = f.commit().await.unwrap();
        assert_eq!(saved.name.as_str(), "a.txt");
        assert_eq!(saved.size, 5);
        assert_eq!(saved.path, dir.path().join("a.txt"));
        assert_eq!(std::fs::read(&saved.path).unwrap(), b"hello");
        let meta = std::fs::metadata(&saved.path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.nlink(), 1, "the staging link is gone");
        assert_eq!(staged_files(dir.path()), 0);
        let staging = std::fs::metadata(dir.path().join(STAGING_DIR)).unwrap();
        assert_eq!(staging.permissions().mode() & 0o777, 0o700);
    }

    #[tokio::test]
    async fn empty_files_are_fine() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        let empty: [u8; 32] = Sha256::digest(b"").into();
        let mut f = inbox.begin(&sanitize("e"), 0, Some(empty)).await.unwrap();
        f.write(b"").await.unwrap();
        let saved = f.commit().await.unwrap();
        assert_eq!(std::fs::read(&saved.path).unwrap(), b"");
    }

    #[tokio::test]
    async fn existing_files_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"mine").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("a (1).txt")).unwrap();
        // A dangling symlink is an existing entry too.
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("a (2).txt"))
            .unwrap();
        let mut f = inbox.begin(&sanitize("a.txt"), 3, None).await.unwrap();
        f.write(b"new").await.unwrap();
        let saved = f.commit().await.unwrap();
        assert_eq!(saved.name.as_str(), "a (3).txt");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"mine");
        assert!(
            std::fs::symlink_metadata(dir.path().join("a (1).txt"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!dir.path().join("nowhere").exists());
    }

    #[tokio::test]
    async fn names_run_out_eventually() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        let name = sanitize("x");
        for n in 0..MAX_NAME_ATTEMPTS {
            std::fs::write(dir.path().join(name.numbered(n).as_str()), b"").unwrap();
        }
        let mut f = inbox.begin(&name, 1, None).await.unwrap();
        f.write(b"1").await.unwrap();
        assert!(matches!(f.commit().await, Err(InboxError::NoFreeName)));
        assert_eq!(staged_files(dir.path()), 0);
    }

    #[tokio::test]
    async fn overflow_is_refused_and_spoils_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        let mut f = inbox.begin(&sanitize("a"), 3, None).await.unwrap();
        assert!(matches!(
            f.write(b"toolong").await,
            Err(InboxError::Overflow)
        ));
        assert_eq!(f.written(), 0);
        // A peer that overran once is not trusted with the rest.
        assert!(f.write(b"abc").await.is_err());
        assert!(f.commit().await.is_err());
        assert_eq!(staged_files(dir.path()), 0);
        assert!(placed(dir.path()).is_empty());

        // Exactly at the declared size is fine; one byte more is not.
        let mut f = inbox.begin(&sanitize("b"), 3, None).await.unwrap();
        f.write(b"ab").await.unwrap();
        assert!(matches!(f.write(b"cd").await, Err(InboxError::Overflow)));
        assert_eq!(f.written(), 2);
        drop(f);
        assert_eq!(staged_files(dir.path()), 0);
        assert!(placed(dir.path()).is_empty());
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
            inbox.begin(&sanitize("a"), u64::MAX, None).await,
            Err(InboxError::TooLarge)
        ));
        assert_eq!(staged_files(dir.path()), 0);
        assert!(matches!(
            inbox.check_space(u64::MAX),
            Err(InboxError::NoSpace)
        ));
        assert!(inbox.check_space(0).is_ok());
        assert!(inbox.available_bytes().is_some());
    }

    #[tokio::test]
    async fn stale_staging_files_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        drop(Inbox::open(dir.path()).unwrap());
        let staging = dir.path().join(STAGING_DIR);
        std::fs::write(staging.join("dead.part"), b"x").unwrap();
        // Only regular `*.part` files: not other names, not directories,
        // not symlinks (whose targets must never be touched).
        std::fs::write(staging.join("keep.txt"), b"x").unwrap();
        std::fs::create_dir(staging.join("dir.part")).unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"x").unwrap();
        std::os::unix::fs::symlink(&victim, staging.join("link.part")).unwrap();
        drop(Inbox::open(dir.path()).unwrap());
        assert!(!staging.join("dead.part").exists());
        assert!(staging.join("keep.txt").exists());
        assert!(staging.join("dir.part").is_dir());
        assert!(std::fs::symlink_metadata(staging.join("link.part")).is_ok());
        assert!(victim.exists());
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
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            Inbox::open(&file),
            Err(InboxError::NotADirectory(_))
        ));
    }

    #[test]
    fn a_symlinked_staging_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&elsewhere, target.join(STAGING_DIR)).unwrap();
        assert!(matches!(
            Inbox::open(&target),
            Err(InboxError::NotADirectory(p)) if p == target.join(STAGING_DIR)
        ));
        assert!(matches!(
            Inbox::open_with_staging(&target, &target.join(STAGING_DIR)),
            Err(InboxError::NotADirectory(_))
        ));
    }

    #[test]
    fn missing_directories_are_created_and_loose_ones_tightened() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Downloads/Sukkula");
        Inbox::open(&target).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&target), 0o700);
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::set_permissions(
            target.join(STAGING_DIR),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        Inbox::open(&target).unwrap();
        // The owner's choice to share reading is kept; writing is not.
        assert_eq!(mode(&target), 0o755);
        assert_eq!(mode(&target.join(STAGING_DIR)), 0o700);
    }

    #[tokio::test]
    async fn a_directory_swapped_mid_transfer_redirects_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let inbox = Inbox::open(&target).unwrap();
        let mut f = inbox.begin(&sanitize("a.txt"), 4, None).await.unwrap();
        f.write(b"da").await.unwrap();
        // Another app swaps the whole target for a symlink to elsewhere.
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let moved = dir.path().join("moved");
        std::fs::rename(&target, &moved).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &target).unwrap();
        f.write(b"ta").await.unwrap();
        let saved = f.commit().await.unwrap();
        // The file went to the directory that was checked, now `moved`.
        assert_eq!(std::fs::read(moved.join("a.txt")).unwrap(), b"data");
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        assert_eq!(saved.name.as_str(), "a.txt");
        // And the next file refuses the symlink outright.
        assert!(matches!(
            inbox.begin(&sanitize("b"), 1, None).await,
            Err(InboxError::NotADirectory(_))
        ));
    }

    #[tokio::test]
    async fn a_deleted_target_is_recreated_for_the_next_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let inbox = Inbox::open(&target).unwrap();
        std::fs::remove_dir(target.join(STAGING_DIR)).unwrap();
        std::fs::remove_dir(&target).unwrap();
        let mut f = inbox.begin(&sanitize("a"), 1, None).await.unwrap();
        f.write(b"x").await.unwrap();
        f.commit().await.unwrap();
        assert_eq!(std::fs::read(target.join("a")).unwrap(), b"x");
    }

    #[tokio::test]
    async fn a_staging_file_swapped_for_a_symlink_is_not_placed() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, b"key!").unwrap();
        let mut f = inbox.begin(&sanitize("photo.jpg"), 4, None).await.unwrap();
        f.write(b"jpeg").await.unwrap();
        let staged = the_staging_file(&dir.path().join(STAGING_DIR));
        std::fs::remove_file(&staged).unwrap();
        std::os::unix::fs::symlink(&secret, &staged).unwrap();
        assert!(f.commit().await.is_err());
        assert_eq!(placed(dir.path()), vec!["secret".to_owned()]);
        assert_eq!(std::fs::read(&secret).unwrap(), b"key!");
    }

    #[tokio::test]
    async fn a_staging_file_grown_behind_our_back_is_not_placed() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        let mut f = inbox.begin(&sanitize("a"), 2, None).await.unwrap();
        f.write(b"ab").await.unwrap();
        let staged = the_staging_file(&dir.path().join(STAGING_DIR));
        let mut other = std::fs::OpenOptions::new()
            .append(true)
            .open(&staged)
            .unwrap();
        std::io::Write::write_all(&mut other, b"extra").unwrap();
        assert!(f.commit().await.is_err());
        assert!(placed(dir.path()).is_empty());
        assert_eq!(staged_files(dir.path()), 0);
    }

    #[tokio::test]
    async fn a_dropped_or_aborted_file_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path()).unwrap();
        // Aborted while waiting for more data.
        let i = inbox.clone();
        let task = tokio::spawn(async move {
            let mut f = i.begin(&sanitize("a"), 10, None).await.unwrap();
            f.write(b"abc").await.unwrap();
            std::future::pending::<()>().await;
        });
        while staged_files(dir.path()) == 0 {
            tokio::task::yield_now().await;
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(staged_files(dir.path()), 0);
        // A panic in the adapter holding it.
        let i = inbox.clone();
        let task = tokio::spawn(async move {
            let mut f = i.begin(&sanitize("b"), 10, None).await.unwrap();
            f.write(b"abc").await.unwrap();
            panic!("adapter bug");
        });
        assert!(task.await.unwrap_err().is_panic());
        assert_eq!(staged_files(dir.path()), 0);
        assert!(placed(dir.path()).is_empty());
    }

    #[test]
    fn only_link_incapable_errors_fall_back_to_copying() {
        for e in [
            Errno::XDEV,
            Errno::PERM,
            Errno::OPNOTSUPP,
            Errno::NOSYS,
            Errno::MLINK,
        ] {
            assert!(copy_instead(e), "{e:?}");
        }
        for e in [
            Errno::EXIST,
            Errno::NOENT,
            Errno::ACCESS,
            Errno::NOSPC,
            Errno::LOOP,
            Errno::IO,
        ] {
            assert!(!copy_instead(e), "{e:?}");
        }
    }

    #[test]
    fn errors_describe_themselves() {
        for e in [
            InboxError::NotADirectory(PathBuf::from("/x")),
            InboxError::TooLarge,
            InboxError::Overflow,
            InboxError::Truncated {
                got: 1,
                declared: 2,
            },
            InboxError::DigestMismatch,
            InboxError::NoSpace,
            InboxError::NoFreeName,
            spoiled(),
            tampered(),
        ] {
            assert!(!e.to_string().is_empty());
        }
    }

    /// A second mount for the staging directory: `/dev/shm` is tmpfs on
    /// ordinary Linux hosts, so a link from there into a temp dir on disk
    /// fails with `EXDEV` for real. `None` when that is not so here.
    fn other_mount(target: &Path) -> Option<tempfile::TempDir> {
        let shm = tempfile::tempdir_in("/dev/shm").ok()?;
        let a = std::fs::metadata(shm.path()).ok()?.dev();
        let b = std::fs::metadata(target).ok()?.dev();
        (a != b).then_some(shm)
    }

    #[tokio::test]
    async fn across_mounts_the_file_is_copied() {
        let dir = tempfile::tempdir().unwrap();
        let Some(shm) = other_mount(dir.path()) else {
            eprintln!("skipped: no second file system for staging");
            return;
        };
        let staging = shm.path().join("staging");
        let inbox = Inbox::open_with_staging(dir.path(), &staging).unwrap();
        let data: Vec<u8> = (0..200_000u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let digest: [u8; 32] = Sha256::digest(&data).into();
        std::fs::write(dir.path().join("big.bin"), b"taken").unwrap();
        let mut f = inbox
            .begin(
                &sanitize("big.bin"),
                u64::try_from(data.len()).unwrap(),
                Some(digest),
            )
            .await
            .unwrap();
        for chunk in data.chunks(65_536) {
            f.write(chunk).await.unwrap();
        }
        let saved = f.commit().await.unwrap();
        assert_eq!(saved.name.as_str(), "big (1).bin");
        assert_eq!(std::fs::read(&saved.path).unwrap(), data);
        let meta = std::fs::symlink_metadata(&saved.path).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read(dir.path().join("big.bin")).unwrap(), b"taken");
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_commit_cancelled_mid_copy_leaves_nothing_half_written() {
        let dir = tempfile::tempdir().unwrap();
        let Some(shm) = other_mount(dir.path()) else {
            eprintln!("skipped: no second file system for staging");
            return;
        };
        let staging = shm.path().join("staging");
        let inbox = Inbox::open_with_staging(dir.path(), &staging).unwrap();
        let size: usize = 32 * 1024 * 1024;
        let chunk = vec![0x5Au8; 1024 * 1024];
        let mut f = inbox
            .begin(&sanitize("big.bin"), u64::try_from(size).unwrap(), None)
            .await
            .unwrap();
        for _ in 0..size / chunk.len() {
            f.write(&chunk).await.unwrap();
        }
        let dest = dir.path().join("big.bin");
        let mut commit = Box::pin(f.commit());
        let mut cx = Context::from_waker(Waker::noop());
        // Drive the commit until the copy has started, then drop it.
        let mut cancelled = false;
        for _ in 0..100_000 {
            match commit.as_mut().poll(&mut cx) {
                Poll::Ready(r) => {
                    // Finished before we could catch it: then it is whole.
                    r.unwrap();
                    break;
                }
                Poll::Pending if dest.exists() => {
                    cancelled = true;
                    break;
                }
                Poll::Pending => tokio::task::yield_now().await,
            }
        }
        drop(commit);
        // Let a copy step already handed to a blocking thread finish.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if cancelled {
            assert!(!dest.exists(), "a half-copied file was left in place");
        } else if dest.exists() {
            assert_eq!(
                std::fs::metadata(&dest).unwrap().len(),
                u64::try_from(size).unwrap()
            );
        }
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
    }
}
