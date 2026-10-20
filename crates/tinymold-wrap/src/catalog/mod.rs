//! Listing what the cache keeps, for garbage collection and upload.
//!
//! The wrapper's records live beside the store's objects: actions under
//! `entries/<crate>/`, pre-key records under `prekeys/`, digest caches
//! under `stat/` and `rustc/`, and compile locks under `locks/`. [`records`] lists them all with their last
//! use, and [`action_references`] says which objects an action needs, so a
//! caller can tell which objects are still referenced.

mod types;

use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

use tinymold_hash::Digest;

use crate::Result;
use crate::error::io;
use crate::record::{self, Action};
use crate::test_record::TestRun;

pub use types::{ActionInfo, RecordFile, RecordKind};

/// Lists every record and cache file in the store at `store`.
///
/// Files that are none of these — names that are not digests outside the
/// cache directories — are left out. A file removed while the walk runs is
/// skipped.
///
/// # Errors
///
/// Returns [`Error::Io`](crate::Error::Io) when a directory exists but cannot
/// be read.
pub fn records(store: &Path) -> Result<Vec<RecordFile>> {
    let mut found = Vec::new();
    for crate_dir in entries(&store.join("entries"))? {
        let path = crate_dir.path();
        let Some(crate_name) = crate_dir.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !path.is_dir() {
            continue;
        }
        for entry in entries(&path)? {
            let kind = match Digest::from_str(&entry.file_name().to_string_lossy()) {
                Ok(key) => RecordKind::Action {
                    crate_name: crate_name.clone(),
                    key,
                },
                Err(_) => match temporary(&entry) {
                    Some(kind) => kind,
                    None => continue,
                },
            };
            push(&mut found, &entry, kind);
        }
    }
    for entry in entries(&store.join("prekeys"))? {
        let kind = match Digest::from_str(&entry.file_name().to_string_lossy()) {
            Ok(key) => RecordKind::PreKey { key },
            Err(_) => match temporary(&entry) {
                Some(kind) => kind,
                None => continue,
            },
        };
        push(&mut found, &entry, kind);
    }
    for (dir, run) in [("run", true), ("bin", false)] {
        for entry in entries(&store.join("tests").join(dir))? {
            let kind = match Digest::from_str(&entry.file_name().to_string_lossy()) {
                Ok(key) if run => RecordKind::TestRun { key },
                Ok(digest) => RecordKind::TestBinary { digest },
                Err(_) => match temporary(&entry) {
                    Some(kind) => kind,
                    None => continue,
                },
            };
            push(&mut found, &entry, kind);
        }
    }
    for dir in ["stat", "rustc"] {
        caches(&store.join(dir), &mut found)?;
    }
    for entry in entries(&store.join("locks"))? {
        push(&mut found, &entry, RecordKind::Lock);
    }
    Ok(found)
}

/// What the action at `path` needs and which unit it compiled; see
/// [`ActionInfo`].
///
/// # Errors
///
/// Returns [`Error::CorruptRecord`](crate::Error::CorruptRecord) when the
/// record is missing or does not parse, and [`Error::Io`](crate::Error::Io)
/// when it cannot be read.
pub fn action_info(path: &Path) -> Result<ActionInfo> {
    let Some(text) = record::read(path)? else {
        return Err(crate::Error::CorruptRecord {
            path: path.to_path_buf(),
            line: 0,
        });
    };
    let action = Action::decode(&text, path)?;
    let unit = action
        .outputs
        .iter()
        .find(|output| {
            output.text
                && Path::new(&output.name)
                    .extension()
                    .is_some_and(|ext| ext == "d")
        })
        .map(|output| output.name.clone());
    Ok(ActionInfo {
        references: action.references(),
        unit,
    })
}

/// Every object the action recorded at `path` needs to be restored: its
/// outputs and its replayed stdout and stderr.
///
/// # Errors
///
/// Returns [`Error::Io`](crate::Error::Io) when the record cannot be read,
/// and [`Error::CorruptRecord`](crate::Error::CorruptRecord) when it does not
/// parse, or is gone.
pub fn action_references(path: &Path) -> Result<Vec<Digest>> {
    Ok(action_info(path)?.references)
}

/// Parses an action record in memory and returns every object it references,
/// including diagnostics and incremental session files. `path` is used only
/// to identify the record in errors; this function performs no I/O.
///
/// # Errors
///
/// Returns [`Error::CorruptRecord`](crate::Error::CorruptRecord) when the
/// record does not parse.
pub fn action_references_from_text(text: &str, path: &Path) -> Result<Vec<Digest>> {
    Ok(Action::decode(text, path)?.references())
}

/// Every object the test run recorded at `path` needs to be replayed: its
/// stdout and stderr.
///
/// # Errors
///
/// Returns [`Error::Io`](crate::Error::Io) when the record cannot be read,
/// and [`Error::CorruptRecord`](crate::Error::CorruptRecord) when it does not
/// parse, or is gone.
pub fn test_run_references(path: &Path) -> Result<Vec<Digest>> {
    let Some(text) = record::read(path)? else {
        return Err(crate::Error::CorruptRecord {
            path: path.to_path_buf(),
            line: 0,
        });
    };
    Ok(TestRun::decode(&text, path)?.references())
}

/// The entries of `dir`; empty when it does not exist.
fn entries(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(entries.filter_map(std::result::Result::ok).collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(io("read", dir)(error)),
    }
}

/// Every file under a cache directory, recursively.
fn caches(dir: &Path, found: &mut Vec<RecordFile>) -> Result<()> {
    for entry in entries(dir)? {
        let path = entry.path();
        if path.is_dir() {
            caches(&path, found)?;
        } else {
            push(found, &entry, RecordKind::Cache);
        }
    }
    Ok(())
}

/// `Temporary` for a record writer's staging file (`.<name>.<pid>.tmp`).
fn temporary(entry: &fs::DirEntry) -> Option<RecordKind> {
    let name = entry.file_name();
    let name = name.to_str()?;
    let staging = name.starts_with('.')
        && Path::new(name)
            .extension()
            .is_some_and(|extension| extension == "tmp");
    staging.then_some(RecordKind::Temporary)
}

fn push(found: &mut Vec<RecordFile>, entry: &fs::DirEntry, kind: RecordKind) {
    let Ok(meta) = entry.metadata() else {
        return;
    };
    let Ok(last_used) = meta.modified() else {
        return;
    };
    if meta.is_file() {
        found.push(RecordFile {
            kind,
            path: entry.path(),
            size: meta.len(),
            last_used,
        });
    }
}

#[cfg(test)]
mod test;
