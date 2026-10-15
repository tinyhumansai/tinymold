//! Unit tests for store garbage collection.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs::File;
use std::time::{Duration, SystemTime};

use super::*;
use crate::{Entry, FrontEnd, Key, Namespace};

const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

fn key(name: &[u8]) -> Key {
    Namespace::new(FrontEnd::Turbo, "ci")
        .unwrap()
        .key(name)
        .unwrap()
}

/// Sets a file's mtime, which is its last use, to `when`.
fn age(path: &Path, when: SystemTime) {
    let file = File::options().read(true).open(path).unwrap();
    file.set_modified(when).unwrap();
}

/// Stores `bytes` under `name`, with entry and object last used at `when`.
fn entry(store: &Store, name: &[u8], bytes: &[u8], when: SystemTime) -> Key {
    let digest = store.ingest(bytes, None).unwrap().digest;
    let key = key(name);
    store.put(&key, &Entry::new(vec![digest])).unwrap();
    age(&store.entry_path(&key), when);
    age(&store.cas().object_path(&digest), when);
    key
}

fn policy(now: SystemTime) -> Policy {
    Policy {
        now,
        ..Policy::garbage_only()
    }
}

#[test]
fn garbage_only_keeps_every_entry_and_what_it_references() {
    let (_dir, store) = store();
    let now = SystemTime::now();
    let old = entry(&store, b"old", b"old bytes", now - 30 * DAY);

    let report = collect(&store, &policy(now)).unwrap();

    assert!(store.get(&old).unwrap().is_some());
    assert_eq!(report.entries_kept, 1);
    assert_eq!(report.entries_removed, 0);
    assert_eq!(report.objects_removed, 0);
}

#[test]
fn entries_unused_for_longer_than_max_age_are_removed_with_their_objects() {
    let (_dir, store) = store();
    let now = SystemTime::now();
    let old = entry(&store, b"old", b"old bytes", now - 30 * DAY);
    let fresh = entry(&store, b"fresh", b"fresh bytes", now - HOUR * 2);

    let report = collect(
        &store,
        &Policy {
            max_age: Some(14 * DAY),
            ..policy(now)
        },
    )
    .unwrap();

    assert!(!store.contains(&old));
    assert!(store.contains(&fresh));
    assert!(!store.cas().contains(&Digest::of_bytes(b"old bytes")));
    assert_eq!(report.entries_removed, 1);
    assert_eq!(report.objects_removed, 1);
    assert_eq!(report.bytes_freed, 9);
}

#[test]
fn the_least_recently_used_entries_go_first_to_fit_the_budget() {
    let (_dir, store) = store();
    let now = SystemTime::now();
    let oldest = entry(&store, b"a", b"aaaa", now - 3 * DAY);
    let middle = entry(&store, b"b", b"bbbb", now - 2 * DAY);
    let newest = entry(&store, b"c", b"cccc", now - DAY);

    let report = collect(
        &store,
        &Policy {
            max_size: Some(8),
            ..policy(now)
        },
    )
    .unwrap();

    assert!(!store.contains(&oldest));
    assert!(store.contains(&middle));
    assert!(store.contains(&newest));
    assert_eq!(report.bytes_kept, 8);
}

#[test]
fn an_object_shared_by_two_entries_survives_evicting_one() {
    let (_dir, store) = store();
    let now = SystemTime::now();
    let gone = entry(&store, b"gone", b"shared", now - 30 * DAY);
    let kept = entry(&store, b"kept", b"shared", now - HOUR * 2);

    collect(
        &store,
        &Policy {
            max_age: Some(14 * DAY),
            ..policy(now)
        },
    )
    .unwrap();

    assert!(!store.contains(&gone));
    let entry = store.get(&kept).unwrap().unwrap();
    assert!(store.cas().contains(&entry.blobs[0]));
}

#[test]
fn an_object_uploaded_within_the_grace_period_survives_without_an_entry() {
    let (_dir, store) = store();
    let now = SystemTime::now();
    // An upload whose entry has not been written yet.
    let pinned = store.ingest(&b"in flight"[..], None).unwrap().digest;
    let stale = store.ingest(&b"abandoned"[..], None).unwrap().digest;
    age(&store.cas().object_path(&stale), now - 2 * HOUR);

    let report = collect(&store, &policy(now)).unwrap();

    assert!(store.cas().contains(&pinned));
    assert!(!store.cas().contains(&stale));
    assert_eq!(report.objects_removed, 1);
}

#[test]
fn the_sha256_index_forgets_objects_that_were_removed() {
    let (_dir, store) = store();
    let now = SystemTime::now();
    let ingested = store.ingest(&b"abandoned"[..], None).unwrap();
    age(&store.cas().object_path(&ingested.digest), now - 2 * HOUR);

    collect(&store, &policy(now)).unwrap();

    assert!(!store.sha256_path(&ingested.sha256).exists());
}

#[test]
fn stale_pending_and_staging_files_are_swept_and_fresh_ones_kept() {
    let (_dir, store) = store();
    let now = SystemTime::now();
    let pending = store.root().join("pending");
    let stale = pending.join("1-0.tmp");
    let fresh = pending.join("1-1.tmp");
    let staging = store.root().join("tmp").join("1-2.tmp");
    for path in [&stale, &fresh, &staging] {
        fs::write(path, b"partial").unwrap();
    }
    age(&stale, now - 2 * HOUR);
    age(&staging, now - 2 * HOUR);

    let report = collect(&store, &policy(now)).unwrap();

    assert!(!stale.exists());
    assert!(!staging.exists());
    assert!(fresh.exists());
    assert_eq!(report.stale_files_removed, 2);
}

#[test]
fn a_malformed_entry_is_removed() {
    let (_dir, store) = store();
    let key = key(b"corrupt");
    let path = store.entry_path(&key);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"garbage").unwrap();

    let report = collect(&store, &policy(SystemTime::now())).unwrap();

    assert!(!path.exists());
    assert_eq!(report.entries_removed, 1);
}

#[test]
fn a_dry_run_reports_without_removing() {
    let (_dir, store) = store();
    let now = SystemTime::now();
    let old = entry(&store, b"old", b"old bytes", now - 30 * DAY);

    let report = collect(
        &store,
        &Policy {
            max_age: Some(14 * DAY),
            dry_run: true,
            ..policy(now)
        },
    )
    .unwrap();

    assert_eq!(report.entries_removed, 1);
    assert_eq!(report.objects_removed, 1);
    assert!(store.contains(&old));
    assert!(store.cas().contains(&Digest::of_bytes(b"old bytes")));
}

#[test]
fn a_read_refreshes_the_last_use_of_an_entry_and_its_objects() {
    let (_dir, store) = store();
    let now = SystemTime::now();
    let key = entry(&store, b"read", b"read bytes", now - 30 * DAY);

    store.get(&key).unwrap().unwrap();
    let report = collect(
        &store,
        &Policy {
            max_age: Some(14 * DAY),
            ..policy(now)
        },
    )
    .unwrap();

    assert!(store.contains(&key));
    assert_eq!(report.entries_removed, 0);
}
