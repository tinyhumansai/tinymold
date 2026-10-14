//! Keeping rustc's incremental sessions with the actions that made them.
//!
//! On a hit rustc does not run, so it leaves no incremental session for the
//! unit. A target directory filled from the store then holds almost none,
//! and the first edit of each crate compiles from scratch: on ripgrep, 2.5 s
//! where one with its session takes 1.0 s (ADR 0008). So a miss of an
//! incremental compile stores the session rustc just wrote with its action,
//! and a hit restores it when the unit's incremental directory has none.
//!
//! - **Which session is the unit's.** Several units of one crate (its library
//!   and its tests) keep sessions side by side, under
//!   `<incremental>/<crate>-<id>/s-<time>-<random>-<hash>`. rustc hardlinks a
//!   session's object files into the out-dir as the unit's outputs, so the
//!   unit's session is the finished one holding a file with the inode of one
//!   of its output objects. Without split debuginfo, rustc keeps no object
//!   files in the out-dir; the session is then the only one of the crate
//!   written during the compile, and none when two were.
//! - **Only at the same path.** rustc discards a session made in another
//!   working directory, so a session is restored only into the directory it
//!   was taken from.
//! - **Never in the way.** A session is a cache rustc validates before using
//!   any of it. Restoring one never fails a hit, and one is restored only
//!   where the unit's incremental directory does not exist, so a session
//!   rustc wrote since is never replaced. Files are placed by reflink or
//!   copy, never hardlinked, because rustc continues a session by writing
//!   into a copy of it.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use tinymold_cas::{Store, Strategy};
use tinymold_hash::Digest;

use crate::Result;
use crate::error::io;
use crate::record::SessionRecord;

/// The unit's finished session under `incremental`: the crate directory
/// `<crate>-<id>` and session name. That is the session holding a file with
/// an inode in `inodes`, the compile's output objects, or else the only
/// session of the crate written at or after `since_ns`, the compile's start
/// on the filesystem clock. With two candidates and no inode to tell them
/// apart, as when a library and its tests compile at once, there is none.
pub(crate) fn find(
    incremental: &Path,
    crate_name: &str,
    inodes: &HashSet<(u64, u64)>,
    since_ns: u64,
) -> Option<(PathBuf, String)> {
    let prefix = format!("{crate_name}-");
    let mut written = Vec::new();
    for crate_dir in fs::read_dir(incremental).ok()?.filter_map(|entry| entry.ok()) {
        if !crate_dir.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let Ok(sessions) = fs::read_dir(crate_dir.path()) else {
            continue;
        };
        for session in sessions.filter_map(|entry| entry.ok()) {
            let name = session.file_name().to_string_lossy().into_owned();
            let Ok(meta) = session.metadata() else {
                continue;
            };
            if !is_finished(&name) || !meta.is_dir() {
                continue;
            }
            if holds_any(&session.path(), inodes) {
                return Some((crate_dir.path(), name));
            }
            if crate::stat::identity_of(&meta).mtime_ns >= since_ns {
                written.push((crate_dir.path(), name));
            }
        }
    }
    (written.len() == 1).then(|| written.remove(0))
}

/// Whether `name` is a finished session directory: `s-<time>-<random>-<hash>`,
/// not a lock file and not one still being written (`-working`).
fn is_finished(name: &str) -> bool {
    name.starts_with("s-")
        && !name.ends_with(".lock")
        && !name.ends_with("-working")
        && name.split('-').count() == 4
}

fn holds_any(dir: &Path, inodes: &HashSet<(u64, u64)>) -> bool {
    !inodes.is_empty()
        && fs::read_dir(dir).is_ok_and(|files| {
        files
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.metadata().ok())
            .any(|meta| inodes.contains(&dev_ino(&meta)))
    })
}

#[cfg(unix)]
pub(crate) fn dev_ino(meta: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
pub(crate) fn dev_ino(_meta: &fs::Metadata) -> (u64, u64) {
    // No inode numbers to match a session by: none is found.
    (0, 0)
}

/// Stores every file of the session `name` in `crate_dir`. A file that is one
/// of the compile's outputs, by inode, takes that output's digest from
/// `known`; the rest (the dep graph, the query cache) are inserted.
///
/// # Errors
///
/// Returns an error when a file cannot be read or stored.
pub(crate) fn store(
    store: &Store,
    crate_dir: &Path,
    name: &str,
    known: &HashMap<(u64, u64), Digest>,
) -> Result<SessionRecord> {
    let dir = crate_dir.join(name);
    let mut files = Vec::new();
    for entry in fs::read_dir(&dir).map_err(io("read", &dir))? {
        let entry = entry.map_err(io("read", &dir))?;
        let meta = entry.metadata().map_err(io("read", &entry.path()))?;
        let Some(file) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let digest = match known.get(&dev_ino(&meta)) {
            Some(digest) if store.keep(digest) => *digest,
            _ => store.insert_file(entry.path())?,
        };
        files.push((file, digest));
    }
    files.sort();
    Ok(SessionRecord {
        dir: crate_dir.to_string_lossy().into_owned(),
        name: name.to_owned(),
        files,
    })
}

/// Restores `session` when it came from a crate directory in `incremental`
/// that does not exist now. Returns whether it did; a failure leaves no
/// partial session behind.
pub(crate) fn restore(store: &Store, session: &SessionRecord, incremental: &Path) -> bool {
    let crate_dir = PathBuf::from(&session.dir);
    if crate_dir.parent() != Some(incremental) || crate_dir.exists() {
        return false;
    }
    // Written beside the crate directory, then renamed into place, so rustc
    // never finds a session with files missing.
    let staging = crate_dir.with_extension("tinymold-restoring");
    let placed = place(store, session, &staging, &crate_dir);
    if placed.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    placed.is_ok()
}

fn place(store: &Store, session: &SessionRecord, staging: &Path, crate_dir: &Path) -> Result<()> {
    let _ = fs::remove_dir_all(staging);
    let dir = staging.join(&session.name);
    fs::create_dir_all(&dir).map_err(io("create", &dir))?;
    for (file, digest) in &session.files {
        let destination = dir.join(file);
        if store
            .materialize(digest, &destination, Strategy::Reflink)
            .is_err()
        {
            store.materialize(digest, &destination, Strategy::Copy)?;
        }
        make_writable(&destination)?;
    }
    // rustc pairs each session with a lock file named for its time and
    // random parts; a session without one is taken for abandoned.
    if let Some((stem, _hash)) = session.name.rsplit_once('-') {
        let lock = staging.join(format!("{stem}.lock"));
        fs::write(&lock, b"").map_err(io("write", &lock))?;
    }
    fs::rename(staging, crate_dir).map_err(io("rename into place", crate_dir))
}

/// Store objects are read-only; rustc rewrites a session's files.
#[cfg(unix)]
fn make_writable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))
        .map_err(io("set the mode of", path))
}

#[cfg(not(unix))]
fn make_writable(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path).map_err(io("read", path))?.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).map_err(io("set the mode of", path))
}

#[cfg(test)]
mod test;
