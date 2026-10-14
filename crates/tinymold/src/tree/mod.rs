//! Cloning a whole directory tree, such as a cargo `target/`, without copying
//! bytes where the filesystem allows it.
//!
//! On APFS a single `clonefile` call clones an entire directory hierarchy:
//! measured at about 7 ms for ripgrep's 200 MB, 456-file `target/`, against
//! 0.57 s for `cp -c -R`, which clones file by file. Elsewhere the tree is
//! walked and each file is reflinked (`FICLONE` on btrfs and XFS) or, failing
//! that, copied.
//!
//! Either way every file keeps its modification time. Cargo decides freshness
//! by comparing source mtimes with output mtimes, so a clone that reset them
//! would make every output look newer than it is.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::SystemTime;

use crate::{Error, Result};

/// How [`clone_tree`] placed a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TreeClone {
    /// One copy-on-write clone of the whole directory (APFS `clonefile`).
    Whole,
    /// A walk that placed each file on its own.
    Files {
        /// Files placed by a copy-on-write clone.
        reflinked: u64,
        /// Files placed by a byte copy.
        copied: u64,
    },
}

impl fmt::Display for TreeClone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Whole => f.write_str("whole-tree clone"),
            Self::Files { reflinked, copied } => {
                write!(f, "{reflinked} files reflinked, {copied} copied")
            }
        }
    }
}

/// Clones the directory `source` to `destination`, preserving every file's
/// modification time.
///
/// `destination` must not exist; its parent must.
///
/// # Errors
///
/// Returns [`Error::NotADirectory`] when `source` is not a directory,
/// [`Error::AlreadyExists`] when `destination` exists, and [`Error::Io`] when
/// reading the source or writing the clone fails. A failed walk may leave a
/// partial `destination` behind.
pub fn clone_tree(source: &Path, destination: &Path) -> Result<TreeClone> {
    clone_tree_with(source, destination, true)
}

/// [`clone_tree`], with the whole-tree clone optional so the per-file walk can
/// be exercised on filesystems where the whole-tree clone always succeeds.
fn clone_tree_with(source: &Path, destination: &Path, whole: bool) -> Result<TreeClone> {
    if !fs::metadata(source).is_ok_and(|meta| meta.is_dir()) {
        return Err(Error::NotADirectory(source.to_path_buf()));
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(Error::AlreadyExists(destination.to_path_buf()));
    }
    if whole && clone_whole(source, destination) {
        return Ok(TreeClone::Whole);
    }
    let mut counts = (0, 0);
    walk(source, destination, true, &mut counts)?;
    Ok(TreeClone::Files {
        reflinked: counts.0,
        copied: counts.1,
    })
}

/// Tries one clone of the whole directory. Only Apple's `clonefile` accepts a
/// directory, so other platforms go straight to the walk.
#[cfg(target_vendor = "apple")]
fn clone_whole(source: &Path, destination: &Path) -> bool {
    if reflink_copy::reflink(source, destination).is_ok() {
        return true;
    }
    // A failed clone may leave a partial directory; it is ours to remove,
    // since `destination` did not exist before the attempt.
    let _ = fs::remove_dir_all(destination);
    false
}

#[cfg(not(target_vendor = "apple"))]
fn clone_whole(_source: &Path, _destination: &Path) -> bool {
    false
}

/// Places every entry of `source` under `destination`, counting files placed
/// by reflink and by copy. `reflink: false` goes straight to copying.
fn walk(source: &Path, destination: &Path, reflink: bool, counts: &mut (u64, u64)) -> Result<()> {
    fs::create_dir(destination).map_err(io_error("create", destination))?;
    let entries = fs::read_dir(source).map_err(io_error("read", source))?;
    for entry in entries {
        let entry = entry.map_err(io_error("read", source))?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let kind = entry.file_type().map_err(io_error("read", &from))?;
        if kind.is_dir() {
            walk(&from, &to, reflink, counts)?;
        } else if kind.is_symlink() {
            copy_symlink(&from, &to)?;
        } else {
            if reflink && reflink_copy::reflink(&from, &to).is_ok() {
                counts.0 += 1;
            } else {
                fs::copy(&from, &to).map_err(io_error("copy", &to))?;
                counts.1 += 1;
            }
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .map_err(io_error("read", &from))?;
            set_modified(&to, modified).map_err(io_error("set the mtime of", &to))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> Result<()> {
    let target = fs::read_link(from).map_err(io_error("read", from))?;
    std::os::unix::fs::symlink(target, to).map_err(io_error("create", to))
}

#[cfg(not(unix))]
fn copy_symlink(from: &Path, to: &Path) -> Result<()> {
    // Creating symlinks on Windows needs a privilege most users lack, so the
    // clone holds a copy of what the link points to instead.
    fs::copy(from, to).map(drop).map_err(io_error("copy", to))
}

/// Sets `path`'s modification time, leaving its access time alone.
pub(crate) fn set_modified(path: &Path, modified: SystemTime) -> io::Result<()> {
    // Windows needs write access to change times; Unix only needs ownership,
    // so a read-only file can still be updated through a read handle.
    let file = match OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => File::open(path)?,
        Err(error) => return Err(error),
    };
    file.set_modified(modified)
}

/// Builds the error-mapping closure for an I/O failure on `path`.
pub(crate) fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io {
        action,
        path,
        source,
    }
}

#[cfg(test)]
mod test;
