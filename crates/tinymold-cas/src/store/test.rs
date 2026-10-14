//! Unit tests for the object store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

#[test]
fn objects_are_sharded_by_their_first_byte() {
    let (_dir, store) = store();
    let digest = Digest::of_bytes(b"");
    let hex = digest.to_hex();

    assert_eq!(
        store.object_path(&digest),
        store.root().join("objects").join(&hex[..2]).join(&hex[2..])
    );
}

#[test]
fn inserted_bytes_read_back_and_are_sealed() {
    let (_dir, store) = store();
    let digest = store.insert_bytes(b"rlib").unwrap();

    assert_eq!(digest, Digest::of_bytes(b"rlib"));
    assert_eq!(store.read(&digest).unwrap(), b"rlib");
    let permissions = fs::metadata(store.object_path(&digest))
        .unwrap()
        .permissions();
    assert!(permissions.readonly());
}

#[test]
fn inserting_the_same_bytes_twice_keeps_one_object_and_no_staging_files() {
    let (_dir, store) = store();
    let first = store.insert_bytes(b"same").unwrap();
    let second = store.insert_bytes(b"same").unwrap();

    assert_eq!(first, second);
    assert_eq!(fs::read_dir(store.tmp_dir()).unwrap().count(), 0);
}

#[test]
fn a_racing_commit_of_existing_bytes_discards_its_staging_file() {
    let (_dir, store) = store();
    let digest = store.insert_bytes(b"raced").unwrap();
    let staging = store.staging_path();
    fs::write(&staging, b"raced").unwrap();

    store.commit(&staging, digest).unwrap();
    assert!(!staging.exists());
}

#[test]
fn inserted_files_are_independent_of_their_source() {
    let (dir, store) = store();
    let source = dir.path().join("libdemo.rlib");
    fs::write(&source, b"original").unwrap();

    let digest = store.insert_file(&source).unwrap();
    fs::write(&source, b"rewritten by cargo").unwrap();

    assert_eq!(store.read(&digest).unwrap(), b"original");
    store.verify(&digest).unwrap();
    assert_eq!(fs::read_dir(store.tmp_dir()).unwrap().count(), 0);
}

#[test]
fn inserting_a_missing_file_fails_cleanly() {
    let (dir, store) = store();
    let missing = dir.path().join("missing");

    let error = store.insert_file(&missing).unwrap_err();
    assert!(matches!(error, Error::Io { action: "read", path, .. } if path == missing));
}

#[test]
fn insert_file_reports_staging_failures_and_cleans_up() {
    let (dir, store) = store();
    let source = dir.path().join("src");
    fs::write(&source, b"bytes").unwrap();
    fs::remove_dir(store.tmp_dir()).unwrap();

    assert!(store.insert_file(&source).is_err());
}

#[test]
fn insert_bytes_reports_an_unwritable_staging_area() {
    let (_dir, store) = store();
    fs::remove_dir(store.tmp_dir()).unwrap();

    let error = store.insert_bytes(b"x").unwrap_err();
    assert!(matches!(
        error,
        Error::Io {
            action: "create",
            ..
        }
    ));
}

#[test]
fn missing_objects_are_reported_by_digest() {
    let (_dir, store) = store();
    let digest = Digest::of_bytes(b"never stored");

    assert!(!store.contains(&digest));
    assert!(matches!(store.read(&digest), Err(Error::NotFound(d)) if d == digest));
    assert!(matches!(store.verify(&digest), Err(Error::NotFound(_))));
}

#[test]
fn verify_detects_a_tampered_object() {
    let (_dir, store) = store();
    let digest = store.insert_bytes(b"trusted").unwrap();
    let path = store.object_path(&digest);
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&path, permissions).unwrap();
    fs::write(&path, b"tampered").unwrap();

    let error = store.verify(&digest).unwrap_err();
    assert!(matches!(
        error,
        Error::Corrupt { expected, actual }
            if expected == digest && actual == Digest::of_bytes(b"tampered")
    ));
}

#[test]
fn materialize_creates_parents_and_refuses_to_overwrite() {
    let (dir, store) = store();
    let digest = store.insert_bytes(b"artifact").unwrap();
    let destination = dir.path().join("target/debug/deps/libdemo.rlib");

    store
        .materialize(&digest, &destination, Strategy::Copy)
        .unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"artifact");

    let error = store
        .materialize(&digest, &destination, Strategy::Copy)
        .unwrap_err();
    assert!(matches!(error, Error::DestinationExists(path) if path == destination));
}

#[test]
fn materialize_reports_a_missing_object() {
    let (dir, store) = store();
    let digest = Digest::of_bytes(b"absent");

    let error = store
        .materialize(&digest, dir.path().join("out"), Strategy::Auto)
        .unwrap_err();
    assert!(matches!(error, Error::NotFound(_)));
}

#[test]
fn opening_fails_when_the_root_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("file");
    fs::write(&root, b"").unwrap();

    assert!(matches!(
        Store::open(&root),
        Err(Error::Io {
            action: "create",
            ..
        })
    ));
}

#[test]
fn objects_lists_every_object_with_its_size() {
    let (_dir, store) = store();
    let small = store.insert_bytes(b"a").unwrap();
    let large = store.insert_bytes(b"bigger").unwrap();

    let mut listed: Vec<_> = store
        .objects()
        .unwrap()
        .into_iter()
        .map(|object| (object.digest, object.size))
        .collect();
    listed.sort();
    let mut expected = vec![(small, 1), (large, 6)];
    expected.sort();
    assert_eq!(listed, expected);
}

#[test]
fn objects_skips_files_that_are_not_objects() {
    let (_dir, store) = store();
    let digest = store.insert_bytes(b"a").unwrap();
    let shard = store.object_path(&digest).parent().unwrap().to_path_buf();
    fs::write(shard.join("not-a-digest"), b"x").unwrap();
    fs::write(store.root().join("objects").join("stray"), b"x").unwrap();

    let listed = store.objects().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].digest, digest);
}

#[test]
fn objects_of_an_empty_store_is_empty() {
    let (_dir, store) = store();
    assert_eq!(store.objects().unwrap(), Vec::new());
}

#[test]
fn remove_deletes_a_sealed_object_and_reports_whether_it_was_there() {
    let (_dir, store) = store();
    let digest = store.insert_bytes(b"a").unwrap();

    assert!(store.remove(&digest).unwrap());
    assert!(!store.contains(&digest));
    assert!(!store.remove(&digest).unwrap());
}

#[test]
fn staging_files_lists_leftovers_and_remove_staging_deletes_them() {
    let (_dir, store) = store();
    let leftover = store.root().join("tmp").join("123-0.tmp");
    fs::write(&leftover, b"half").unwrap();

    let staged = store.staging_files().unwrap();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].path, leftover);
    assert_eq!(staged[0].size, 4);

    store.remove_staging(&staged[0]).unwrap();
    assert_eq!(store.staging_files().unwrap(), Vec::new());
}

#[test]
fn listing_a_store_whose_objects_directory_is_a_file_fails() {
    let (_dir, store) = store();
    fs::remove_dir_all(store.root().join("objects")).unwrap();
    fs::write(store.root().join("objects"), b"x").unwrap();
    assert!(matches!(store.objects(), Err(Error::Io { .. })));
}

#[cfg(unix)]
#[test]
fn remove_reports_an_object_it_cannot_delete() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, store) = store();
    let digest = store.insert_bytes(b"a").unwrap();
    let shard = store.object_path(&digest).parent().unwrap().to_path_buf();
    fs::set_permissions(&shard, fs::Permissions::from_mode(0o555)).unwrap();

    let removed = store.remove(&digest);
    fs::set_permissions(&shard, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        removed,
        Err(Error::Io {
            action: "remove",
            ..
        })
    ));
    assert!(store.contains(&digest));
}
