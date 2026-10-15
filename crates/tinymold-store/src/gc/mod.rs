//! Garbage collection for a store: last use, a budget, and safe sweeps.
//!
//! Every read and write sets the mtime of the entry file and of each object
//! it names, so an mtime is a last use, as for the wrapper's records
//! (`docs/specs/gc.md`). [`collect`] removes, in order:
//!
//! 1. entries that do not decode, and entries unused for longer than
//!    [`Policy::max_age`];
//! 2. the least recently used remaining entries, while the objects they
//!    reference exceed [`Policy::max_size`];
//! 3. objects no remaining entry references and not stored or used within
//!    the grace period;
//! 4. SHA-256 index files whose object is gone, and temporaries older than
//!    the grace period.
//!
//! The grace period is what pins an upload in progress: its objects are
//! committed before its entry and look unreferenced until then. Collection
//! only ever deletes, so it cannot cause a wrong hit. A reader whose entry
//! names a removed object gets a miss.
//!
//! A store root collected here must hold only this store's entries. The
//! wrapper's local store keeps its own records and has its own collector,
//! `tinymold gc`, and the two are never pointed at one root.

mod types;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tinymold_hash::Digest;

pub use types::{GRACE, Policy, Report};

use crate::disk::{io_error, read_if_present};
use crate::{Entry, Error, Result, Store};

/// One entry that may be kept.
struct Live {
    path: PathBuf,
    last_used: SystemTime,
    blobs: Vec<Digest>,
}

/// Collects garbage in `store` according to `policy`.
///
/// # Errors
///
/// Returns [`Error::Cas`] or [`Error::Io`] when the store cannot be listed
/// or something that should be removed cannot be. Files that vanish during
/// the walk are skipped.
pub fn collect(store: &Store, policy: &Policy) -> Result<Report> {
    let mut report = Report::default();

    let mut live = Vec::new();
    for (path, last_used) in files_in_shards(&store.root().join("ac"))? {
        let Some(bytes) = read_if_present(&path)? else {
            continue;
        };
        match Entry::decode(&bytes) {
            Ok(entry) if !policy.older_than(last_used, policy.max_age) => live.push(Live {
                path,
                last_used,
                blobs: entry.blobs,
            }),
            _ => {
                remove(&path, policy)?;
                report.entries_removed += 1;
            }
        }
    }

    let objects: HashMap<Digest, (u64, SystemTime)> = store
        .cas()
        .objects()?
        .into_iter()
        .map(|object| (object.digest, (object.size, object.modified)))
        .collect();
    let references = fit_budget(&mut live, &objects, policy, &mut report)?;
    report.entries_kept = live.len() as u64;

    for (digest, (size, modified)) in &objects {
        if references.contains_key(digest) {
            report.objects_kept += 1;
            report.bytes_kept += size;
        } else if policy.older_than(*modified, Some(policy.grace)) {
            if !policy.dry_run {
                store.cas().remove(digest)?;
            }
            report.objects_removed += 1;
            report.bytes_freed += size;
        } else {
            report.objects_kept += 1;
            report.bytes_kept += size;
        }
    }

    for (path, modified) in files_in_shards(&store.root().join("sha256"))? {
        let target = read_if_present(&path)?
            .and_then(|bytes| <[u8; Digest::LEN]>::try_from(bytes.as_slice()).ok())
            .map(Digest::from_bytes);
        let orphaned = match target {
            Some(digest) => !store.cas().contains(&digest),
            None => policy.older_than(modified, Some(policy.grace)),
        };
        // In a dry run nothing was removed, so only already-orphaned index
        // files count.
        if orphaned {
            remove(&path, policy)?;
            report.stale_files_removed += 1;
        }
    }

    for (path, modified) in files_in(&store.root().join("pending"))? {
        if policy.older_than(modified, Some(policy.grace)) {
            remove(&path, policy)?;
            report.stale_files_removed += 1;
        }
    }
    for file in store.cas().staging_files()? {
        if policy.older_than(file.modified, Some(policy.grace)) {
            if !policy.dry_run {
                store.cas().remove_staging(&file)?;
            }
            report.stale_files_removed += 1;
        }
    }

    Ok(report)
}

/// Evicts the least recently used entries until the objects the rest
/// reference fit the budget. Returns how many kept entries reference each
/// object.
fn fit_budget(
    live: &mut Vec<Live>,
    objects: &HashMap<Digest, (u64, SystemTime)>,
    policy: &Policy,
    report: &mut Report,
) -> Result<HashMap<Digest, u64>> {
    let size = |digest: &Digest| objects.get(digest).map_or(0, |(size, _)| *size);
    let mut references: HashMap<Digest, u64> = HashMap::new();
    let mut referenced_bytes = 0;
    for entry in live.iter() {
        for blob in &entry.blobs {
            let count = references.entry(*blob).or_default();
            if *count == 0 {
                referenced_bytes += size(blob);
            }
            *count += 1;
        }
    }

    let Some(budget) = policy.max_size else {
        return Ok(references);
    };
    // Newest first, so the oldest entry is popped from the end.
    live.sort_by_key(|entry| std::cmp::Reverse(entry.last_used));
    while referenced_bytes > budget {
        let Some(entry) = live.pop() else { break };
        remove(&entry.path, policy)?;
        report.entries_removed += 1;
        for blob in &entry.blobs {
            if let Some(count) = references.get_mut(blob) {
                *count -= 1;
                if *count == 0 {
                    references.remove(blob);
                    referenced_bytes -= size(blob);
                }
            }
        }
    }
    Ok(references)
}

/// Every file one level below the shard directories of `dir`, with its
/// mtime.
fn files_in_shards(dir: &Path) -> Result<Vec<(PathBuf, SystemTime)>> {
    let mut files = Vec::new();
    for shard in read_dir(dir)? {
        if shard.file_type().is_ok_and(|kind| kind.is_dir()) {
            files.extend(files_in(&shard.path())?);
        }
    }
    Ok(files)
}

/// Every file directly in `dir`, with its mtime.
fn files_in(dir: &Path) -> Result<Vec<(PathBuf, SystemTime)>> {
    let mut files = Vec::new();
    for entry in read_dir(dir)? {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_file() {
            let modified = meta
                .modified()
                .map_err(io_error("inspect", &entry.path()))?;
            files.push((entry.path(), modified));
        }
    }
    Ok(files)
}

fn read_dir(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(entries.filter_map(std::result::Result::ok).collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(Error::Io {
            action: "read",
            path: dir.to_path_buf(),
            source,
        }),
    }
}

fn remove(path: &Path, policy: &Policy) -> Result<()> {
    if policy.dry_run {
        return Ok(());
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            action: "remove",
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod test;
