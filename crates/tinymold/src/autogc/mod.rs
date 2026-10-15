//! Collecting the store in the background, so it stops growing edit by edit.
//!
//! Every edit of a crate stores a new action for the same unit, and its
//! rlib, binary and test executables are new objects. Nothing removes the
//! older generations until `tinymold gc` runs, so a store used only through
//! the wrapper grows by tens of megabytes an edit. After a wrapper call,
//! [`spawn_if_due`] therefore starts `tinymold gc --keep-generations 3` in the
//! background, at most once per [`INTERVAL`] for each store, unless
//! `TINYMOLD_AUTO_GC=0`.
//!
//! The collection runs detached, with no standard streams, so cargo neither
//! waits for it nor sees its output. Like every collection it only deletes,
//! with the usual grace period for objects a build is storing right now,
//! so the worst it can do is make a later build miss. Two wrappers that find
//! a collection due at the same moment may both start one; collections
//! tolerate each other.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

/// The least time between two automatic collections of one store.
pub const INTERVAL: Duration = Duration::from_secs(600);

/// How many recent generations of each unit an automatic collection keeps.
pub const KEEP_GENERATIONS: usize = 3;

/// The file whose mtime says when the last automatic collection started.
fn stamp(store: &Path) -> PathBuf {
    store.join("gc-stamp")
}

/// Whether an automatic collection of `store` is due at `now`, claiming it
/// when it is by moving the stamp's mtime to `now`. Not due when the store
/// does not exist yet, or when its last collection started within
/// [`INTERVAL`].
#[must_use]
pub fn claim(store: &Path, now: SystemTime) -> bool {
    if !store.is_dir() {
        return false;
    }
    let path = stamp(store);
    if let Ok(last) = fs::metadata(&path).and_then(|meta| meta.modified()) {
        // A stamp from the future, after a clock change, counts as recent.
        let recent = !now
            .duration_since(last)
            .is_ok_and(|elapsed| elapsed >= INTERVAL);
        if recent {
            return false;
        }
    }
    let touched = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .and_then(|file| file.set_modified(now));
    touched.is_ok()
}

/// Whether automatic collection is on: unless `TINYMOLD_AUTO_GC` is `0`,
/// `false`, `no` or `off`.
#[must_use]
pub fn enabled(lookup: impl Fn(&str) -> Option<OsString>) -> bool {
    !lookup("TINYMOLD_AUTO_GC").is_some_and(|value| {
        matches!(
            value.to_string_lossy().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
}

/// The command line an automatic collection of `store` runs.
#[must_use]
pub fn arguments(store: &Path) -> Vec<OsString> {
    vec![
        "--store".into(),
        store.into(),
        "gc".into(),
        "--keep-generations".into(),
        KEEP_GENERATIONS.to_string().into(),
    ]
}

/// Starts a background collection of `store` when one is enabled and due.
/// Best effort: a collection that cannot start is simply skipped.
pub fn spawn_if_due(store: &Path, lookup: impl Fn(&str) -> Option<OsString>) {
    if !enabled(lookup) || !claim(store, SystemTime::now()) {
        return;
    }
    let Ok(program) = std::env::current_exe() else {
        return;
    };
    let _ = Command::new(program)
        .args(arguments(store))
        // Not a wrapper call, whatever the parent was.
        .env_remove("RUSTC_WRAPPER")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod test;
