//! S3: directories held open by descriptor.
//!
//! A path is only a question to the file system, and the answer can change
//! between asking and acting: check that `~/Downloads/Sukkula/.partial` is a
//! plain directory, and another app with the same `Downloads` permission can
//! swap it for a symlink before the next call follows it. So the inbox and
//! the store check a directory once, through the descriptor they then keep
//! using, and do everything else relative to it (`openat`, `linkat`,
//! `unlinkat`, `renameat`). Whatever the path points to later, the writes go
//! to the directory that was checked.
//!
//! Every directory is opened `O_DIRECTORY | O_NOFOLLOW`: a symlink in the last
//! component is refused, not followed. Components above it are the caller's
//! configuration (`~/Downloads`, the app's data directory) and are followed
//! as the system resolves them.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use rustix::fs::{CWD, Mode, OFlags, Stat};
use rustix::io::Errno;

/// How much of a directory's mode may be shared with other users.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Share {
    /// The download directory: other users may read it if its owner said
    /// so, but nobody else may write to it, or they could swap entries under
    /// the inbox. Group and other write bits are cleared.
    ReadOnly,
    /// Staging and the app data directory (S9): `0700`, nothing else.
    Nothing,
}

/// Why a directory could not be used.
#[derive(Debug)]
pub(crate) enum DirError {
    /// A symlink, not a directory, or owned by another user.
    NotPlain,
    /// The file system said no.
    Io(io::Error),
}

impl From<Errno> for DirError {
    fn from(e: Errno) -> Self {
        match e {
            // O_NOFOLLOW on a symlink, O_DIRECTORY on anything else.
            Errno::LOOP | Errno::NOTDIR => DirError::NotPlain,
            e => DirError::Io(e.into()),
        }
    }
}

impl From<io::Error> for DirError {
    fn from(e: io::Error) -> Self {
        DirError::Io(e)
    }
}

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Opens `path` as a private directory, creating it (and any missing parents)
/// `0700` first if it does not exist.
pub(crate) fn open(path: &Path, share: Share) -> Result<OwnedFd, DirError> {
    let fd = match rustix::fs::openat(CWD, path, DIR_FLAGS, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => {
            // mkdir never follows a symlink in its last component, and the
            // open below refuses one that appeared since.
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
            rustix::fs::openat(CWD, path, DIR_FLAGS, Mode::empty())?
        }
        Err(e) => return Err(e.into()),
    };
    check(fd, share)
}

/// Opens `name` inside the directory `parent` as a private directory,
/// creating it `0700` first if it does not exist. `name` is one component.
pub(crate) fn open_at(parent: &OwnedFd, name: &str, share: Share) -> Result<OwnedFd, DirError> {
    let fd = match rustix::fs::openat(parent, name, DIR_FLAGS, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => {
            match rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(e) => return Err(e.into()),
            }
            rustix::fs::openat(parent, name, DIR_FLAGS, Mode::empty())?
        }
        Err(e) => return Err(e.into()),
    };
    check(fd, share)
}

/// Checks an open directory: ours, and not writable by anyone else.
fn check(fd: OwnedFd, share: Share) -> Result<OwnedFd, DirError> {
    let st = rustix::fs::fstat(&fd)?;
    if !is_dir(&st) || st.st_uid != rustix::process::geteuid().as_raw() {
        return Err(DirError::NotPlain);
    }
    let mode = st.st_mode & 0o7777;
    let wanted = match share {
        Share::ReadOnly => mode & !0o022,
        Share::Nothing => mode & 0o700,
    };
    if wanted != mode {
        // Through the descriptor: `chmod(path)` would follow a symlink put
        // there since the check.
        rustix::fs::fchmod(fd.as_fd(), Mode::from_raw_mode(wanted))?;
    }
    Ok(fd)
}

fn is_dir(st: &Stat) -> bool {
    rustix::fs::FileType::from_raw_mode(st.st_mode) == rustix::fs::FileType::Directory
}

/// A file's identity: device and inode.
pub(crate) type FileId = (u64, u64);

/// The identity of what `st` describes.
pub(crate) fn file_id(st: &Stat) -> FileId {
    (st.st_dev, st.st_ino)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Tests set the scene with plain writes.
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn missing_directories_are_created_private() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b/c");
        open(&deep, Share::Nothing).unwrap();
        assert_eq!(mode(&deep), 0o700);
        assert_eq!(mode(&dir.path().join("a")), 0o700);
        let root = open(dir.path(), Share::ReadOnly).unwrap();
        open_at(&root, "nested", Share::Nothing).unwrap();
        assert_eq!(mode(&dir.path().join("nested")), 0o700);
        // A second open finds it.
        open_at(&root, "nested", Share::Nothing).unwrap();
    }

    #[test]
    fn modes_are_tightened_through_the_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("shared");
        std::fs::create_dir(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o777)).unwrap();
        open(&p, Share::ReadOnly).unwrap();
        assert_eq!(mode(&p), 0o755);
        open(&p, Share::Nothing).unwrap();
        assert_eq!(mode(&p), 0o700);
    }

    #[test]
    fn symlinks_and_files_are_not_directories() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(matches!(
            open(&link, Share::Nothing),
            Err(DirError::NotPlain)
        ));
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            open(&file, Share::Nothing),
            Err(DirError::NotPlain)
        ));
        let root = open(dir.path(), Share::ReadOnly).unwrap();
        assert!(matches!(
            open_at(&root, "link", Share::Nothing),
            Err(DirError::NotPlain)
        ));
        assert!(matches!(
            open_at(&root, "file", Share::Nothing),
            Err(DirError::NotPlain)
        ));
        // A dangling symlink is not an invitation to create its target.
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("dangling"))
            .unwrap();
        assert!(matches!(
            open_at(&root, "dangling", Share::Nothing),
            Err(DirError::NotPlain)
        ));
        assert!(!dir.path().join("nowhere").exists());
    }

    /// Only testable where the tests may give a directory away: as root, as
    /// in the CI container. Elsewhere it says so and passes.
    #[test]
    fn a_directory_owned_by_someone_else_is_refused() {
        if !rustix::process::geteuid().is_root() {
            eprintln!("skipped: not root, cannot chown");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let theirs = dir.path().join("theirs");
        std::fs::create_dir(&theirs).unwrap();
        rustix::fs::chown(&theirs, Some(rustix::fs::Uid::from_raw(4242)), None).unwrap();
        assert!(matches!(open(&theirs, Share::Nothing), Err(DirError::NotPlain)));
        let root = open(dir.path(), Share::ReadOnly).unwrap();
        assert!(matches!(
            open_at(&root, "theirs", Share::Nothing),
            Err(DirError::NotPlain)
        ));
        // And its mode is left alone: not ours to change.
        std::fs::set_permissions(&theirs, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(open(&theirs, Share::Nothing).is_err());
        assert_eq!(mode(&theirs), 0o777);
    }

    #[test]
    fn io_errors_are_reported_as_such() {
        // Creating under a regular file fails with ENOTDIR from mkdir, which
        // is a "not plain" for the open and an I/O error for the create.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(open(&file.join("sub"), Share::Nothing).is_err());
        let e: DirError = Errno::ACCESS.into();
        assert!(matches!(e, DirError::Io(_)));
        let e: DirError = io::Error::other("x").into();
        assert!(matches!(e, DirError::Io(_)));
    }
}
