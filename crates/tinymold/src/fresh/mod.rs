//! Content freshness: undo mtime changes that did not change content.
//!
//! Cargo decides whether a unit is stale by comparing the mtimes of its
//! sources with the mtime of its last build. A checkout, a rebase, a branch
//! switch, a fresh `git worktree`, or a plain `touch` gives files new mtimes
//! without changing a byte, and cargo rebuilds everything that reads them. On
//! ripgrep that costs 0.84 s, more than a real one-line edit (0.52 s).
//!
//! [`record`] writes a manifest of every workspace file's mtime, size and
//! BLAKE3 digest into the target directory, right after a build. [`restore`]
//! later puts the recorded mtime back on every file whose content still
//! matches, so cargo sees exactly the timestamps it built from. Files that
//! did change keep their new mtime and rebuild as usual.
//!
//! # Why this is safe
//!
//! A restore only reproduces a state cargo has already seen: the mtimes the
//! workspace had when the target directory last matched it. That holds only
//! while the target directory is unchanged since the record, so the manifest
//! carries a digest of cargo's fingerprint files (their paths and mtimes).
//! Cargo rewrites a unit's fingerprint whenever it builds that unit, so any
//! build in between changes the digest and the restore refuses
//! ([`Restore::TargetChanged`]). Only fingerprints are read, not the outputs
//! themselves: about 600 files for bat's 9,667-file `target/`. Without that check, an edit, a build,
//! and an undo would get the undone file an old mtime that cargo would wrongly
//! judge older than the edited build's outputs.
//!
//! The manifest lives in the target directory (`<target>/.tinymold/fresh`),
//! so it travels with a cloned or snapshotted `target/`.

mod types;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tinymold_hash::{Digest, Hasher};

use crate::tree::{io_error, set_modified};
use crate::{Error, Result};

use types::{Entry, Manifest};
pub use types::{RecordReport, Restore, RestoreReport};

/// The directory inside a target directory where tinymold keeps its files.
const STATE_DIR: &str = ".tinymold";

/// Where the freshness manifest for `target` lives.
#[must_use]
pub fn manifest_path(target: &Path) -> PathBuf {
    target.join(STATE_DIR).join("fresh")
}

/// Records the current mtime, size and digest of every workspace file into
/// `target`'s manifest.
///
/// Run it after a build. Files whose size and mtime match the previous
/// manifest, and were last modified before it was written, reuse its digest
/// instead of being re-read.
///
/// The walk skips `.git`, the target directory, any directory holding a
/// `CACHEDIR.TAG` (other cargo target directories), and any nested directory
/// with its own `.git` (submodules and worktrees, which are workspaces of
/// their own).
///
/// # Errors
///
/// Returns [`Error::Io`] when the workspace or target cannot be read or the
/// manifest cannot be written, and [`Error::Hash`] when a file cannot be
/// hashed.
pub fn record(workspace: &Path, target: &Path) -> Result<RecordReport> {
    let recorded_ns = nanos(SystemTime::now());
    // The previous manifest is only a digest cache: an unreadable or
    // outdated one is ignored, not an error.
    let previous: HashMap<String, Entry> = load(target)
        .ok()
        .flatten()
        .map(|manifest| {
            let cutoff = manifest.recorded_ns;
            manifest
                .entries
                .into_iter()
                // Git's racily-clean rule: a file modified at or after the
                // previous record may have changed within one timestamp tick
                // of being hashed, so it is hashed again.
                .filter(|entry| entry.mtime_ns < cutoff)
                .map(|entry| (entry.path.clone(), entry))
                .collect()
        })
        .unwrap_or_default();

    let mut files = Vec::new();
    let mut skipped = 0;
    let excluded = fs::canonicalize(target).ok();
    walk_workspace(
        workspace,
        workspace,
        excluded.as_deref(),
        &mut files,
        &mut skipped,
    )?;

    let mut hashed = 0;
    let mut entries = Vec::with_capacity(files.len());
    for (path, rel, meta) in files {
        let size = meta.len();
        let mtime_ns = nanos(meta.modified().map_err(io_error("read", &path))?);
        let digest = match previous.get(&rel) {
            Some(old) if old.size == size && old.mtime_ns == mtime_ns => old.digest,
            _ => {
                hashed += 1;
                Digest::of_file(&path)?
            }
        };
        entries.push(Entry {
            path: rel,
            size,
            mtime_ns,
            digest,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let state = target_state(target)?;
    let manifest = Manifest {
        recorded_ns,
        stamp_ns: state.newest_ns,
        fingerprints: state.fingerprints,
        entries,
    };
    write_atomically(&manifest_path(target), manifest.encode().as_bytes())?;
    Ok(RecordReport {
        files: manifest.entries.len(),
        hashed,
        skipped,
    })
}

/// Puts back the recorded mtime of every workspace file whose content still
/// matches `target`'s manifest.
///
/// A file whose content differs from the manifest but whose mtime is not
/// newer than the target's outputs gets a new mtime, so cargo cannot mistake
/// outputs built from other content for current ones.
///
/// # Errors
///
/// Returns [`Error::CorruptManifest`] when the manifest cannot be parsed,
/// [`Error::Io`] when the target cannot be read or an mtime cannot be set,
/// and [`Error::Hash`] when a file cannot be hashed.
pub fn restore(workspace: &Path, target: &Path) -> Result<Restore> {
    apply(workspace, target, None)
}

/// Digests of workspace files by manifest path, `None` for a file that could
/// not be hashed. Shared between snapshot scoring and the restore after it.
pub(crate) type Digests = HashMap<String, Option<Digest>>;

/// [`restore`], optionally verifying every file by content.
///
/// With `verify`, a file whose size and mtime match the manifest is still
/// hashed (memoized in the map), which is what a target cloned in from
/// another workspace needs: equal mtimes there say nothing about content.
pub(crate) fn apply(
    workspace: &Path,
    target: &Path,
    verify: Option<&mut Digests>,
) -> Result<Restore> {
    let Some(manifest) = load(target)? else {
        return Ok(Restore::NoManifest);
    };
    if target_state(target)?.fingerprints != manifest.fingerprints {
        return Ok(Restore::TargetChanged);
    }
    let mut verify = verify;
    let mut report = RestoreReport::default();
    for entry in &manifest.entries {
        let path = workspace.join(&entry.path);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => meta,
            _ => {
                report.missing += 1;
                continue;
            }
        };
        let mtime_ns = nanos(meta.modified().map_err(io_error("read", &path))?);
        let same = if meta.len() != entry.size {
            false
        } else if let Some(digests) = verify.as_deref_mut() {
            let digest = *digests
                .entry(entry.path.clone())
                .or_insert_with(|| Digest::of_file(&path).ok());
            digest == Some(entry.digest)
        } else if mtime_ns == entry.mtime_ns {
            true
        } else {
            Digest::of_file(&path)? == entry.digest
        };
        if !same {
            report.changed += 1;
            if mtime_ns <= manifest.stamp_ns {
                // Older than the outputs, so cargo would call them current.
                let newer = nanos(SystemTime::now()).max(manifest.stamp_ns + 1_000_000_000);
                set_modified(&path, from_nanos(newer))
                    .map_err(io_error("set the mtime of", &path))?;
            }
        } else if mtime_ns == entry.mtime_ns {
            report.unchanged += 1;
        } else {
            set_modified(&path, from_nanos(entry.mtime_ns))
                .map_err(io_error("set the mtime of", &path))?;
            report.restored += 1;
        }
    }
    Ok(Restore::Applied(report))
}

/// Reads `target`'s manifest, or `None` when it has none.
pub(crate) fn load(target: &Path) -> Result<Option<Manifest>> {
    let path = manifest_path(target);
    match fs::read_to_string(&path) {
        Ok(text) => Manifest::decode(&text, &path).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Io {
            action: "read",
            path,
            source: error,
        }),
    }
}

/// A workspace file found by the walk: its path, its path relative to the
/// workspace root, and its metadata.
type Found = (PathBuf, String, fs::Metadata);

fn walk_workspace(
    root: &Path,
    dir: &Path,
    target: Option<&Path>,
    files: &mut Vec<Found>,
    skipped: &mut usize,
) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(io_error("read", dir))?;
    for entry in entries {
        let entry = entry.map_err(io_error("read", dir))?;
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        let kind = entry.file_type().map_err(io_error("read", &path))?;
        if kind.is_dir() {
            if path.join("CACHEDIR.TAG").exists()
                || path.join(".git").exists()
                || target
                    .is_some_and(|target| fs::canonicalize(&path).ok().as_deref() == Some(target))
            {
                continue;
            }
            walk_workspace(root, &path, target, files, skipped)?;
        } else if kind.is_file() {
            match relative(root, &path) {
                Some(rel) => {
                    let meta = entry.metadata().map_err(io_error("read", &path))?;
                    files.push((path, rel, meta));
                }
                None => *skipped += 1,
            }
        }
    }
    Ok(())
}

/// `path` relative to `root` with `/` separators, or `None` when it cannot be
/// written as one manifest line.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect();
    let joined = parts?.join("/");
    (!joined.contains('\n') && !joined.contains('\r')).then_some(joined)
}

/// What a target directory's cargo fingerprints say about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TargetState {
    /// The newest fingerprint mtime, in nanoseconds; 0 when there are none.
    pub(crate) newest_ns: u64,
    /// Digest of every fingerprint file's path and mtime.
    pub(crate) fingerprints: Digest,
}

/// Reads `target`'s cargo fingerprints.
///
/// Cargo keeps one fingerprint directory per unit, at
/// `<profile>/.fingerprint/<unit>/` (and `<profile>/build/<pkg>/<hash>/fingerprint/`
/// in build-dir layout v2), with `<profile>` one or two levels below the
/// target root (`debug`, or `<triple>/debug`). It rewrites a unit's
/// fingerprint after every build of that unit and deletes it before
/// rebuilding, while a no-op build leaves them all alone. A missing target
/// has no fingerprints.
pub(crate) fn target_state(target: &Path) -> Result<TargetState> {
    let mut found = Vec::new();
    for profile in profile_dirs(target)? {
        collect_fingerprints(target, &profile.join(".fingerprint"), &mut found)?;
        for package in subdirs(&profile.join("build"))? {
            for unit in subdirs(&package)? {
                collect_fingerprints(target, &unit.join("fingerprint"), &mut found)?;
            }
        }
    }
    found.sort();
    let mut hasher = Hasher::new();
    let mut newest_ns = 0;
    for (path, mtime_ns) in &found {
        hasher.field(path.as_bytes()).field(&mtime_ns.to_le_bytes());
        newest_ns = newest_ns.max(*mtime_ns);
    }
    Ok(TargetState {
        newest_ns,
        fingerprints: hasher.finish(),
    })
}

/// Directories one and two levels below `target`, where cargo's profile
/// directories live; tinymold's own state directory is not one.
fn profile_dirs(target: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for first in subdirs(target)? {
        if first.file_name().is_some_and(|name| name == STATE_DIR) {
            continue;
        }
        dirs.extend(subdirs(&first)?);
        dirs.push(first);
    }
    Ok(dirs)
}

/// The subdirectories of `dir`; none when `dir` does not exist or is not a
/// directory.
fn subdirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(io_error("read", dir)(error)),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_error("read", dir))?;
        if entry
            .file_type()
            .map_err(io_error("read", &entry.path()))?
            .is_dir()
        {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

/// Adds every file under the fingerprint directory `dir` to `found`, as its
/// path relative to `target` and its mtime.
fn collect_fingerprints(target: &Path, dir: &Path, found: &mut Vec<(String, u64)>) -> Result<()> {
    for unit in subdirs(dir)?
        .into_iter()
        .chain(std::iter::once(dir.to_path_buf()))
    {
        let Ok(entries) = fs::read_dir(&unit) else {
            continue;
        };
        for entry in entries {
            let entry = entry.map_err(io_error("read", &unit))?;
            let path = entry.path();
            let meta = entry.metadata().map_err(io_error("read", &path))?;
            if meta.is_file() {
                let modified = meta.modified().map_err(io_error("read", &path))?;
                let rel = path.strip_prefix(target).unwrap_or(&path);
                found.push((rel.to_string_lossy().replace('\\', "/"), nanos(modified)));
            }
        }
    }
    Ok(())
}

/// Writes `bytes` to `path` through a temporary file and a rename, so a
/// reader never sees half a manifest.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(io_error("create", dir))?;
    let temp = dir.join(format!(".fresh.{}.tmp", std::process::id()));
    fs::write(&temp, bytes).map_err(io_error("write", &temp))?;
    fs::rename(&temp, path).map_err(io_error("rename into place", path))
}

/// Nanoseconds since the Unix epoch; times before it count as 0, and times
/// past the year 2554 saturate.
fn nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
    })
}

fn from_nanos(nanos: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(nanos)
}

#[cfg(test)]
mod test;
