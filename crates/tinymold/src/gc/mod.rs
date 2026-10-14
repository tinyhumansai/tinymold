//! `tinymold gc`: removing what builds no longer use.
//!
//! Every hit sets the mtime of its action and pre-key records, so a record's
//! mtime is its last use (see `docs/specs/gc.md`). [`collect`] removes:
//!
//! 1. actions unused for longer than [`Policy::max_age`];
//! 2. the least recently used remaining actions, while the objects they
//!    reference exceed [`Policy::max_size`];
//! 3. pre-key records and digest caches older than `max_age`, and record
//!    temporaries older than the grace period;
//! 4. objects no remaining action references and older than the grace
//!    period, and staging files older than it;
//! 5. snapshots older than [`Policy::snapshots_max_age`].
//!
//! Collection only ever deletes, so it cannot cause a wrong hit. An action
//! whose object was removed fails to restore, and the wrapper then treats it
//! as a miss. The grace period protects a compile being stored right now:
//! its objects are written before its action, and would otherwise look
//! unreferenced.

mod types;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use tinymold_cas::Store;
use tinymold_hash::Digest;
use tinymold_wrap::{RecordFile, RecordKind};

use crate::{Error, Result, snapshot};

pub use types::{Age, ByteSize, Report, StoreUsage};

/// How long an unreferenced object or a temporary file is left alone, in
/// case the process that wrote it is still working.
pub const GRACE: Duration = Duration::from_secs(3600);

/// What [`collect`] may remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Remove actions, pre-key records and digest caches not used for this
    /// long. `None` keeps them whatever their age.
    pub max_age: Option<Duration>,
    /// Evict the least recently used actions while the objects they reference
    /// take more than this many bytes. `None` sets no budget.
    pub max_size: Option<u64>,
    /// Remove snapshots older than this. `None` keeps them.
    pub snapshots_max_age: Option<Duration>,
    /// Report what would be removed, but remove nothing.
    pub dry_run: bool,
    /// The time ages are measured from.
    pub now: SystemTime,
    /// How old an unreferenced object or temporary must be to be removed.
    pub grace: Duration,
}

impl Policy {
    /// A policy that removes only garbage: unreferenced objects and stale
    /// temporaries, measured from now with the standard [`GRACE`].
    #[must_use]
    pub fn garbage_only() -> Self {
        Self {
            max_age: None,
            max_size: None,
            snapshots_max_age: None,
            dry_run: false,
            now: SystemTime::now(),
            grace: GRACE,
        }
    }

    /// Whether something last used at `when` is older than `age`.
    fn older_than(&self, when: SystemTime, age: Option<Duration>) -> bool {
        age.is_some_and(|age| {
            self.now
                .duration_since(when)
                .is_ok_and(|elapsed| elapsed > age)
        })
    }
}

/// An action that may be kept.
struct Action {
    record: RecordFile,
    references: Vec<Digest>,
}

/// Collects garbage in the store at `store` according to `policy`.
///
/// # Errors
///
/// Returns [`Error::Wrap`] or [`Error::Cas`] when the store cannot be listed,
/// and [`Error::Io`] or [`Error::Cas`] when something that should be removed
/// cannot be. Files that vanish during the walk are skipped.
pub fn collect(store: &Path, policy: &Policy) -> Result<Report> {
    let mut report = Report {
        dry_run: policy.dry_run,
        ..Report::default()
    };
    let objects = Store::open(store)?;
    let sizes: HashMap<Digest, (u64, SystemTime)> = objects
        .objects()?
        .into_iter()
        .map(|object| (object.digest, (object.size, object.modified)))
        .collect();

    let mut actions = Vec::new();
    for record in tinymold_wrap::records(store)? {
        match &record.kind {
            RecordKind::Action { .. } => {
                match tinymold_wrap::action_references(&record.path) {
                    Ok(references) if !policy.older_than(record.last_used, policy.max_age) => {
                        actions.push(Action { record, references });
                    }
                    // Expired, corrupt, or gone since the walk.
                    _ => {
                        remove_file(&record.path, policy)?;
                        report.actions_removed += 1;
                    }
                }
            }
            RecordKind::PreKey { .. } | RecordKind::Cache => {
                if policy.older_than(record.last_used, policy.max_age) {
                    remove_file(&record.path, policy)?;
                    if matches!(record.kind, RecordKind::PreKey { .. }) {
                        report.prekeys_removed += 1;
                    } else {
                        report.caches_removed += 1;
                    }
                }
            }
            _ => {
                if policy.older_than(record.last_used, Some(policy.grace)) {
                    remove_file(&record.path, policy)?;
                    report.caches_removed += 1;
                }
            }
        }
    }

    let referenced = fit_budget(&mut actions, &sizes, policy, &mut report)?;
    report.actions_kept = actions.len() as u64;

    for (digest, (size, modified)) in &sizes {
        if referenced.contains_key(digest) {
            report.objects_kept.objects += 1;
            report.objects_kept.bytes.0 += size;
        } else if policy.older_than(*modified, Some(policy.grace)) {
            if !policy.dry_run {
                objects.remove(digest)?;
            }
            report.objects_removed.objects += 1;
            report.objects_removed.bytes.0 += size;
        } else {
            // Too young to tell from an ingest in progress.
            report.objects_kept.objects += 1;
            report.objects_kept.bytes.0 += size;
        }
    }
    for staged in objects.staging_files()? {
        if policy.older_than(staged.modified, Some(policy.grace)) {
            if !policy.dry_run {
                objects.remove_staging(&staged)?;
            }
            report.caches_removed += 1;
        }
    }

    report.snapshots_removed = collect_snapshots(store, policy)?;
    Ok(report)
}

/// Evicts the least recently used `actions` until the objects the rest
/// reference fit `policy.max_size`, and returns how many kept actions
/// reference each object.
fn fit_budget(
    actions: &mut Vec<Action>,
    sizes: &HashMap<Digest, (u64, SystemTime)>,
    policy: &Policy,
    report: &mut Report,
) -> Result<HashMap<Digest, u64>> {
    let mut references: HashMap<Digest, u64> = HashMap::new();
    let mut total = 0;
    for action in actions.iter() {
        for digest in &action.references {
            let count = references.entry(*digest).or_insert(0);
            if *count == 0 {
                total += sizes.get(digest).map_or(0, |(size, _)| *size);
            }
            *count += 1;
        }
    }
    let Some(budget) = policy.max_size else {
        return Ok(references);
    };
    // Newest first, so the oldest can be popped off the end.
    actions.sort_by_key(|action| std::cmp::Reverse(action.record.last_used));
    while total > budget {
        let Some(oldest) = actions.pop() else {
            break;
        };
        for digest in &oldest.references {
            if let Some(count) = references.get_mut(digest) {
                *count -= 1;
                if *count == 0 {
                    references.remove(digest);
                    total -= sizes.get(digest).map_or(0, |(size, _)| *size);
                }
            }
        }
        remove_file(&oldest.record.path, policy)?;
        report.actions_removed += 1;
    }
    Ok(references)
}

/// Removes snapshots older than `policy.snapshots_max_age`, judged by when
/// their freshness manifest was recorded, and returns how many.
fn collect_snapshots(store: &Path, policy: &Policy) -> Result<u64> {
    if policy.snapshots_max_age.is_none() {
        return Ok(0);
    }
    let dir = snapshot::snapshots_dir(store);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(Error::Io {
                action: "read",
                path: dir,
                source,
            });
        }
    };
    let mut removed = 0;
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if !path.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let manifest = crate::fresh::manifest_path(&path);
        let Ok(saved) = fs::metadata(&manifest)
            .or_else(|_| fs::metadata(&path))
            .and_then(|meta| meta.modified())
        else {
            continue;
        };
        if policy.older_than(saved, policy.snapshots_max_age) {
            if !policy.dry_run {
                fs::remove_dir_all(&path).map_err(|source| Error::Io {
                    action: "remove",
                    path: path.clone(),
                    source,
                })?;
            }
            removed += 1;
        }
    }
    Ok(removed)
}

/// How many objects the store at `store` holds, and their bytes; nothing
/// when there is no store there yet, which this does not create.
///
/// # Errors
///
/// Returns [`Error::Cas`] when the store cannot be listed.
pub fn usage(store: &Path) -> Result<StoreUsage> {
    let mut usage = StoreUsage::default();
    if !store.join("objects").is_dir() {
        return Ok(usage);
    }
    for object in Store::open(store)?.objects()? {
        usage.objects += 1;
        usage.bytes.0 += object.size;
    }
    Ok(usage)
}

fn remove_file(path: &Path, policy: &Policy) -> Result<()> {
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
