//! Placing store objects at destination paths without copying when possible.
//!
//! A cache hit is only cheap if restoring the artifact is cheap. A plain copy
//! of a 600 MB rlib reads and writes 1.2 GB, while a copy-on-write clone or a
//! hardlink touches only metadata. [`Strategy::Auto`] tries them in order of
//! safety and speed:
//!
//! 1. **Reflink** — `clonefile` on APFS, `FICLONE` on btrfs and XFS. Data
//!    blocks are shared until either side writes, so the result is an
//!    independent, writable file at almost no cost.
//! 2. **Hardlink** — a second name for the store's inode. Free on every
//!    filesystem, but the file *is* the store object, so it stays read-only:
//!    a tool that rewrites its output in place (as mold does) must not be
//!    handed one.
//! 3. **Copy** — the portable fallback, used across filesystems.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use crate::{Error, Result};

/// How [`Store::materialize`](crate::Store::materialize) may place an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Strategy {
    /// Reflink, then hardlink, then copy: the first one the platform allows.
    #[default]
    Auto,
    /// Copy-on-write clone only; fail if the filesystem cannot clone.
    Reflink,
    /// Hardlink only; fail across filesystems. The result is read-only.
    Hardlink,
    /// Byte copy only.
    Copy,
}

impl Strategy {
    /// The methods this strategy tries, in order.
    const fn methods(self) -> &'static [Method] {
        match self {
            Self::Auto => &[Method::Reflink, Method::Hardlink, Method::Copy],
            Self::Reflink => &[Method::Reflink],
            Self::Hardlink => &[Method::Hardlink],
            Self::Copy => &[Method::Copy],
        }
    }
}

/// The method that actually placed an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Method {
    /// A copy-on-write clone sharing data blocks with the store.
    Reflink,
    /// A hardlink to the store object itself.
    Hardlink,
    /// An independent byte copy.
    Copy,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Reflink => "reflink",
            Self::Hardlink => "hardlink",
            Self::Copy => "copy",
        })
    }
}

/// Places `source` at `destination` with the first method `strategy` allows.
///
/// `destination` must not exist and its parent directory must.
pub(crate) fn place(source: &Path, destination: &Path, strategy: Strategy) -> Result<Method> {
    place_with(source, destination, strategy.methods())
}

/// Tries each method in `methods` in turn; the error reported on total
/// failure is the last method's, since that is the most portable one tried.
fn place_with(source: &Path, destination: &Path, methods: &[Method]) -> Result<Method> {
    let mut failure = None;
    for &method in methods {
        match try_method(source, destination, method) {
            Ok(()) => return Ok(method),
            Err(source) => failure = Some((method, source)),
        }
    }

    let (method, source) =
        failure.unwrap_or_else(|| (Method::Copy, io::Error::from(io::ErrorKind::InvalidInput)));
    Err(Error::Unsupported {
        method,
        path: destination.to_path_buf(),
        source,
    })
}

fn try_method(source: &Path, destination: &Path, method: Method) -> io::Result<()> {
    match method {
        Method::Reflink => {
            reflink_copy::reflink(source, destination)?;
            // The clone inherits the store object's read-only mode, but it is
            // an independent file the caller owns, so give the owner write
            // access back.
            make_owner_writable(destination)
        }
        Method::Hardlink => fs::hard_link(source, destination),
        Method::Copy => {
            let mut reader = File::open(source)?;
            // `create_new` refuses to follow or replace anything already at
            // the destination, closing the gap between the caller's existence
            // check and this write. A fresh file gets default, writable
            // permissions rather than the store's read-only mode.
            let mut writer = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?;
            io::copy(&mut reader, &mut writer).map(drop)
        }
    }
}

#[cfg(unix)]
fn make_owner_writable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_owner_writable(path: &Path) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    // Outside Unix there is no owner/other split: clearing the read-only
    // attribute is exactly "owner may write", which is what this lint's
    // world-writable concern does not apply to.
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)
}

#[cfg(test)]
mod test;
