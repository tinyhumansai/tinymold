//! Listing the files a test run may read: its package tree and the inputs
//! the package declares.
//!
//! Reads at run time cannot be traced portably, so a test is assumed to
//! read anything under its package root, as `docs/specs/test-cache.md`
//! specifies. A listing names each file by its path relative to the package
//! root and its content digest, which comes from the stat cache, so an
//! unchanged file is not read again.
//!
//! Left out: `.git`, any directory holding a `CACHEDIR.TAG` (cargo marks
//! target directories with one), and the directories holding the test
//! binary itself. A symbolic link is named by its target and not followed.

use std::fs;
use std::io;
use std::path::Path;

use tinymold_hash::Digest;

use crate::Result;
use crate::error::io;
use crate::stat;

/// Every file under `root`, the package root, apart from what the module
/// docs leave out, sorted by path. `binary` is the test binary, whose
/// directories are skipped.
pub(crate) fn package_files(
    store: &Path,
    root: &Path,
    binary: &Path,
    now_ns: u64,
) -> Result<Vec<(String, Digest)>> {
    let mut files = Vec::new();
    walk(store, root, "", binary, now_ns, &mut files)?;
    files.sort();
    Ok(files)
}

/// The files `declared` names, each relative to `root`: a file, or every
/// file under a directory. A path that does not exist is listed as
/// `<path>!missing`, so that creating it changes the listing.
pub(crate) fn declared_files(
    store: &Path,
    root: &Path,
    declared: &[String],
    now_ns: u64,
) -> Result<Vec<(String, Digest)>> {
    let mut files = Vec::new();
    for path in declared {
        files.extend(named_files(store, &root.join(path), path, now_ns)?);
    }
    Ok(files)
}

/// The file at `path`, or every file under it when it is a directory,
/// listed under `name` rather than under its absolute path, so the listing
/// is the same in every worktree. Missing, it is listed as `<name>!missing`.
pub(crate) fn named_files(
    store: &Path,
    path: &Path,
    name: &str,
    now_ns: u64,
) -> Result<Vec<(String, Digest)>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let mut found = Vec::new();
            walk(store, path, name, Path::new(""), now_ns, &mut found)?;
            found.sort();
            Ok(found)
        }
        Ok(meta) => Ok(vec![(
            name.to_owned(),
            entry_digest(store, path, &meta, now_ns)?,
        )]),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(vec![(format!("{name}!missing"), Digest::of_bytes(b""))])
        }
        Err(error) => Err(io("read", path)(error)),
    }
}

fn walk(
    store: &Path,
    dir: &Path,
    prefix: &str,
    binary: &Path,
    now_ns: u64,
    files: &mut Vec<(String, Digest)>,
) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(io("read", dir))?;
    for entry in entries {
        let entry = entry.map_err(io("read", dir))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(&path).map_err(io("read", &path))?;
        if meta.is_dir() {
            let skipped =
                name == ".git" || path.join("CACHEDIR.TAG").exists() || binary.starts_with(&path);
            if !skipped {
                walk(store, &path, &relative, binary, now_ns, files)?;
            }
        } else {
            files.push((relative, entry_digest(store, &path, &meta, now_ns)?));
        }
    }
    Ok(())
}

/// The digest of a file's content, or of a symbolic link's target.
fn entry_digest(store: &Path, path: &Path, meta: &fs::Metadata, now_ns: u64) -> Result<Digest> {
    if meta.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(io("read", path))?;
        return Ok(Digest::of_bytes(
            format!("symlink:{}", target.to_string_lossy()).as_bytes(),
        ));
    }
    stat::digest(store, path, now_ns)
}

#[cfg(test)]
mod test;
