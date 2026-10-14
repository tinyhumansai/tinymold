//! Whole `target/` snapshots: a new worktree or a branch switch starts warm.
//!
//! A second worktree of the same commit rebuilds every crate from scratch
//! (3.8 s for ripgrep, 13.2 s for bat), because cargo shares nothing between
//! target directories. A snapshot is a copy-on-write clone of a target
//! directory kept in the store under a name, by default the commit it was
//! built from.
//!
//! [`save`] records the workspace's freshness manifest (see
//! [`fresh`]) and clones the target directory into
//! `<store>/snapshots/<name>`. [`restore`] clones a snapshot into a target
//! directory and then runs a freshness restore, so every workspace file whose
//! content matches the snapshot gets the mtime its outputs were built from.
//! Cargo then finds those units fresh, and rebuilds only what actually
//! differs. On APFS the clone is one `clonefile` call, so a restore costs
//! milliseconds plus a no-op cargo build.
//!
//! Choosing a snapshot never affects correctness. Each file is checked by
//! content, so restoring the "wrong" snapshot only means more rebuilds. With
//! no name, [`restore`] picks the snapshot whose manifest matches the most
//! workspace files.
//!
//! Disk is cheap and seconds are not (`docs/adr/0005-trade-disk-for-time.md`),
//! so snapshots are kept until removed. Clones share data blocks with the
//! target they came from until either side is rewritten.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tinymold_hash::Digest;

use crate::fresh::{self, Restore};
use crate::tree::{TreeClone, clone_tree, io_error};
use crate::{Error, Result};

/// A saved snapshot, as listed by [`list`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Snapshot {
    /// The snapshot's name.
    pub name: String,
    /// Files in its freshness manifest; 0 when it has none.
    pub files: usize,
}

/// What [`restore`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Restored {
    /// The snapshot that was restored.
    pub name: String,
    /// Recorded files whose content matches the workspace.
    pub matched: usize,
    /// Files in the snapshot's manifest.
    pub files: usize,
    /// How the target directory was placed.
    pub clone: TreeClone,
    /// The freshness restore that followed; always [`Restore::Applied`].
    pub fresh: Restore,
    /// When `--replace` moved the previous target directory into the store
    /// instead of deleting it, the snapshot name it was kept under.
    pub kept: Option<String>,
}

/// The directory in the store that holds every snapshot.
#[must_use]
pub fn snapshots_dir(store: &Path) -> PathBuf {
    store.join("snapshots")
}

/// Records `workspace`'s freshness manifest into `target`, then saves a clone
/// of `target` as the snapshot `name`, replacing any snapshot of that name.
///
/// # Errors
///
/// Returns [`Error::InvalidSnapshotName`] for a bad name, any error of
/// [`fresh::record`] or [`clone_tree`], and [`Error::Io`] when the store
/// cannot be written.
pub fn save(store: &Path, name: &str, workspace: &Path, target: &Path) -> Result<TreeClone> {
    check_name(name)?;
    fresh::record(workspace, target)?;
    let dir = snapshots_dir(store);
    fs::create_dir_all(&dir).map_err(io_error("create", &dir))?;
    // Clone beside the final name, then rename, so a reader never sees a
    // half-cloned snapshot and an old snapshot of the same name survives a
    // failed save.
    let staging = dir.join(format!(".saving-{name}-{}", std::process::id()));
    remove_tree(&staging)?;
    let clone = clone_tree(target, &staging)?;
    let destination = dir.join(name);
    remove_tree(&destination)?;
    fs::rename(&staging, &destination).map_err(io_error("rename into place", &destination))?;
    Ok(clone)
}

/// Lists every snapshot in `store`, sorted by name.
///
/// # Errors
///
/// Returns [`Error::Io`] when the snapshots directory cannot be read, and
/// [`Error::CorruptManifest`] when a snapshot's manifest cannot be parsed.
pub fn list(store: &Path) -> Result<Vec<Snapshot>> {
    let dir = snapshots_dir(store);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error("read", &dir)(error)),
    };
    let mut snapshots = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_error("read", &dir))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if check_name(&name).is_err() {
            continue;
        }
        let files = fresh::load(&entry.path())?.map_or(0, |m| m.entries.len());
        snapshots.push(Snapshot { name, files });
    }
    snapshots.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(snapshots)
}

/// Restores a snapshot into `target` and makes the workspace's unchanged files
/// fresh against it.
///
/// With `name`, that snapshot is used. Without one, the snapshot whose
/// manifest matches the most workspace files wins. Every file is checked by
/// content, and a file that differs from the snapshot is given an mtime newer
/// than the snapshot's outputs, so cargo rebuilds what depends on it.
///
/// When `target` exists it is an error unless `replace` is set. Replacing
/// keeps the old target as a snapshot named `auto-<fingerprint digest>` when its own
/// manifest is still valid and it can be moved into the store without a copy,
/// so switching back to it later is just as fast. Otherwise it is deleted.
///
/// # Errors
///
/// Returns [`Error::SnapshotNotFound`] or [`Error::NoSnapshots`] when there is
/// nothing to restore, [`Error::AlreadyExists`] when `target` exists and
/// `replace` is not set, [`Error::SnapshotUnusable`] when the snapshot's
/// manifest does not describe its outputs, and any error of [`clone_tree`]
/// or [`fresh::restore`].
pub fn restore(
    store: &Path,
    name: Option<&str>,
    workspace: &Path,
    target: &Path,
    replace: bool,
) -> Result<Restored> {
    let mut digests = fresh::Digests::new();
    let (name, matched, files) = match name {
        Some(name) => {
            check_name(name)?;
            let source = snapshots_dir(store).join(name);
            if !source.is_dir() {
                return Err(Error::SnapshotNotFound(name.to_owned()));
            }
            let (matched, files) = score(&source, workspace, &mut digests)?;
            (name.to_owned(), matched, files)
        }
        None => best_match(store, workspace, &mut digests)?,
    };
    let mut kept = None;
    if fs::symlink_metadata(target).is_ok() {
        if !replace {
            return Err(Error::AlreadyExists(target.to_path_buf()));
        }
        kept = set_aside(store, target)?;
    }
    let clone = clone_tree(&snapshots_dir(store).join(&name), target)?;
    let fresh = fresh::apply(workspace, target, Some(&mut digests))?;
    if !matches!(fresh, Restore::Applied(_)) {
        // Without a valid manifest the clone's mtimes say nothing about the
        // workspace, and cargo could take its outputs for current ones.
        remove_tree(target)?;
        return Err(Error::SnapshotUnusable(name));
    }
    Ok(Restored {
        name,
        matched,
        files,
        clone,
        fresh,
        kept,
    })
}

/// Moves an existing target directory out of the way: into the store as a
/// snapshot when that is valid and free, otherwise into the bin.
fn set_aside(store: &Path, target: &Path) -> Result<Option<String>> {
    let state = fresh::target_state(target)?;
    let valid = matches!(
        fresh::load(target),
        Ok(Some(manifest)) if manifest.fingerprints == state.fingerprints
    );
    if valid {
        // Named by what cargo built, so the same outputs set aside twice are
        // kept once.
        let name = format!("auto-{}", &state.fingerprints.to_hex()[..16]);
        let dir = snapshots_dir(store);
        let destination = dir.join(&name);
        if destination.exists() {
            // The same outputs are already kept under this name.
            remove_tree(target)?;
            return Ok(Some(name));
        }
        if fs::create_dir_all(&dir).is_ok() && fs::rename(target, &destination).is_ok() {
            return Ok(Some(name));
        }
    }
    remove_tree(target)?;
    Ok(None)
}

/// The snapshot matching the most workspace files; ties go to the name that
/// sorts last, so a later `main-2` beats `main-1`.
fn best_match(
    store: &Path,
    workspace: &Path,
    digests: &mut fresh::Digests,
) -> Result<(String, usize, usize)> {
    let mut best: Option<(String, usize, usize)> = None;
    for snapshot in list(store)? {
        let dir = snapshots_dir(store).join(&snapshot.name);
        let (matched, files) = score(&dir, workspace, digests)?;
        if best.as_ref().is_none_or(|(_, top, _)| matched >= *top) {
            best = Some((snapshot.name, matched, files));
        }
    }
    best.ok_or(Error::NoSnapshots)
}

/// How many of the snapshot's recorded files match the workspace by content,
/// out of how many it records. `digests` memoizes workspace hashes across
/// snapshots.
fn score(
    snapshot: &Path,
    workspace: &Path,
    digests: &mut fresh::Digests,
) -> Result<(usize, usize)> {
    let Some(manifest) = fresh::load(snapshot)? else {
        return Ok((0, 0));
    };
    let mut matched = 0;
    for entry in &manifest.entries {
        let path = workspace.join(&entry.path);
        let size = fs::metadata(&path)
            .ok()
            .filter(fs::Metadata::is_file)
            .map(|m| m.len());
        if size != Some(entry.size) {
            continue;
        }
        let digest = *digests
            .entry(entry.path.clone())
            .or_insert_with(|| Digest::of_file(&path).ok());
        if digest == Some(entry.digest) {
            matched += 1;
        }
    }
    Ok((matched, manifest.entries.len()))
}

/// Accepts names made of ASCII letters, digits, `.`, `_` and `-` that do not
/// start with `.`, so a name is always one plain path component.
fn check_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidSnapshotName(name.to_owned()))
    }
}

/// Removes `path` and everything under it; a missing path is not an error.
fn remove_tree(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("remove", path)(error)),
    }
}

#[cfg(test)]
mod test;
