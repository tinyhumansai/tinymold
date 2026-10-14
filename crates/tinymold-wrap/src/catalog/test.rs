//! Unit tests for listing the cache's records.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use super::*;
use crate::record::Output;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn kinds(store: &Path) -> Vec<(RecordKind, PathBuf)> {
    let mut found: Vec<_> = records(store)
        .unwrap()
        .into_iter()
        .map(|record| (record.kind, record.path))
        .collect();
    found.sort_by(|a, b| a.1.cmp(&b.1));
    found
}

#[test]
fn records_lists_actions_pre_keys_caches_locks_and_temporaries() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path();
    let key = Digest::of_bytes(b"key");
    let pre = Digest::of_bytes(b"pre");
    let action = store.join("entries/demo").join(key.to_hex());
    let prekey = store.join("prekeys").join(pre.to_hex());
    let stat = store.join("stat/ab/cdef");
    let rustc = store.join("rustc/0123");
    let temp = store.join("entries/demo/.abc.42.tmp");
    let lock = store.join("locks").join(key.to_hex());
    for path in [&action, &prekey, &stat, &rustc, &temp, &lock] {
        write(path, "x");
    }
    write(&store.join("entries/demo/README"), "not a record");
    write(&store.join("entries/stray-file"), "not a crate directory");

    assert_eq!(
        kinds(store),
        vec![
            (RecordKind::Temporary, temp),
            (
                RecordKind::Action {
                    crate_name: "demo".to_owned(),
                    key,
                },
                action,
            ),
            (RecordKind::Lock, lock),
            (RecordKind::PreKey { key: pre }, prekey),
            (RecordKind::Cache, rustc),
            (RecordKind::Cache, stat),
        ]
    );
}

#[test]
fn records_of_an_empty_store_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(records(dir.path()).unwrap(), Vec::new());
}

#[test]
fn records_reports_size_and_last_use() {
    let dir = tempfile::tempdir().unwrap();
    let pre = Digest::of_bytes(b"pre");
    let path = dir.path().join("prekeys").join(pre.to_hex());
    write(&path, "12345");
    let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(when)
        .unwrap();

    let found = records(dir.path()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].size, 5);
    assert_eq!(found[0].last_used, when);
}

#[test]
fn records_fails_when_a_directory_cannot_be_read() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("prekeys"),
        "a file where a directory belongs",
    );
    assert!(matches!(
        records(dir.path()),
        Err(crate::Error::Io { action: "read", .. })
    ));
}

#[test]
fn action_references_names_outputs_and_replayed_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("action");
    let action = Action {
        stdout: Digest::of_bytes(b"out"),
        stderr: Digest::of_bytes(b"err"),
        outputs: vec![Output {
            name: "libdemo.rlib".to_owned(),
            digest: Digest::of_bytes(b"rlib"),
            executable: false,
            text: false,
        }],
    };
    write(&path, &action.encode());

    assert_eq!(
        action_references(&path).unwrap(),
        vec![
            Digest::of_bytes(b"out"),
            Digest::of_bytes(b"err"),
            Digest::of_bytes(b"rlib"),
        ]
    );
}

#[test]
fn action_references_rejects_a_corrupt_or_missing_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("action");
    assert!(matches!(
        action_references(&path),
        Err(crate::Error::CorruptRecord { line: 0, .. })
    ));
    write(&path, "garbage\n");
    assert!(matches!(
        action_references(&path),
        Err(crate::Error::CorruptRecord { line: 1, .. })
    ));
}
