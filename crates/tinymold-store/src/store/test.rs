//! Unit tests for the store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::FrontEnd;

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

fn key(front_end: FrontEnd, client_key: &[u8]) -> Key {
    Namespace::new(front_end, "ci")
        .unwrap()
        .key(client_key)
        .unwrap()
}

#[test]
fn ingest_names_a_body_by_both_digests() {
    let (_dir, store) = store();
    let ingested = store.ingest(&b"artifact"[..], None).unwrap();

    assert_eq!(ingested.digest, Digest::of_bytes(b"artifact"));
    assert_eq!(ingested.sha256, Sha256::of_bytes(b"artifact"));
    assert_eq!(ingested.size, 8);
    assert!(store.cas().contains(&ingested.digest));
}

#[test]
fn the_sha256_index_finds_an_ingested_body() {
    let (_dir, store) = store();
    let ingested = store.ingest(&b"artifact"[..], None).unwrap();

    assert_eq!(
        store.find_sha256(&Sha256::of_bytes(b"artifact")).unwrap(),
        Some(ingested.digest)
    );
    assert_eq!(store.find_sha256(&Sha256::of_bytes(b"other")).unwrap(), None);
}

#[test]
fn the_sha256_index_misses_once_the_object_is_gone() {
    let (_dir, store) = store();
    let ingested = store.ingest(&b"artifact"[..], None).unwrap();
    store.cas().remove(&ingested.digest).unwrap();

    assert_eq!(store.find_sha256(&ingested.sha256).unwrap(), None);
}

#[test]
fn a_matching_claim_is_accepted() {
    let (_dir, store) = store();
    let by_sha = Claim::Sha256(Sha256::of_bytes(b"artifact"));
    let by_blake = Claim::Blake3(Digest::of_bytes(b"artifact"));

    assert!(store.ingest(&b"artifact"[..], Some(by_sha)).is_ok());
    assert!(store.ingest(&b"artifact"[..], Some(by_blake)).is_ok());
}

#[test]
fn a_body_that_does_not_match_its_sha256_claim_is_refused_and_not_stored() {
    let (_dir, store) = store();
    let claim = Claim::Sha256(Sha256::of_bytes(b"what was claimed"));
    let error = store.ingest(&b"what arrived"[..], Some(claim)).unwrap_err();

    assert!(matches!(error, Error::DigestMismatch { .. }), "{error:?}");
    assert!(!store.cas().contains(&Digest::of_bytes(b"what arrived")));
    assert_eq!(store.find_sha256(&Sha256::of_bytes(b"what arrived")).unwrap(), None);
    assert!(store.cas().staging_files().unwrap().is_empty());
}

#[test]
fn a_body_that_does_not_match_its_blake3_claim_is_refused() {
    let (_dir, store) = store();
    let claim = Claim::Blake3(Digest::of_bytes(b"what was claimed"));
    let error = store.ingest(&b"what arrived"[..], Some(claim)).unwrap_err();

    assert!(matches!(error, Error::DigestMismatch { .. }), "{error:?}");
    assert!(!store.cas().contains(&Digest::of_bytes(b"what arrived")));
}

#[test]
fn an_entry_reads_back_after_it_is_put() {
    let (_dir, store) = store();
    let blob = store.ingest(&b"output"[..], None).unwrap().digest;
    let key = key(FrontEnd::Turbo, b"hash");
    let mut entry = Entry::new(vec![blob]);
    entry.meta.insert("x-artifact-duration".into(), "5".into());

    assert_eq!(store.get(&key).unwrap(), None);
    store.put(&key, &entry).unwrap();

    assert!(store.contains(&key));
    assert_eq!(store.get(&key).unwrap(), Some(entry));
}

#[test]
fn a_put_replaces_the_previous_entry() {
    let (_dir, store) = store();
    let first = store.ingest(&b"first"[..], None).unwrap().digest;
    let second = store.ingest(&b"second"[..], None).unwrap().digest;
    let key = key(FrontEnd::Gradle, b"task");

    store.put(&key, &Entry::new(vec![first])).unwrap();
    store.put(&key, &Entry::new(vec![second])).unwrap();

    assert_eq!(store.get(&key).unwrap().unwrap().blobs, [second]);
}

#[test]
fn an_entry_naming_a_missing_object_is_refused() {
    let (_dir, store) = store();
    let absent = Digest::of_bytes(b"never stored");
    let key = key(FrontEnd::Nx, b"hash");
    let error = store.put(&key, &Entry::new(vec![absent])).unwrap_err();

    assert!(matches!(error, Error::MissingObject(digest) if digest == absent));
    assert!(!store.contains(&key));
}

#[test]
fn the_same_client_key_in_two_namespaces_holds_two_entries() {
    let (_dir, store) = store();
    let a = store.ingest(&b"a"[..], None).unwrap().digest;
    let b = store.ingest(&b"b"[..], None).unwrap().digest;
    let turbo = key(FrontEnd::Turbo, b"same");
    let nx = key(FrontEnd::Nx, b"same");

    store.put(&turbo, &Entry::new(vec![a])).unwrap();
    store.put(&nx, &Entry::new(vec![b])).unwrap();

    assert_eq!(store.get(&turbo).unwrap().unwrap().blobs, [a]);
    assert_eq!(store.get(&nx).unwrap().unwrap().blobs, [b]);
}

#[test]
fn a_corrupt_entry_file_is_reported_as_malformed() {
    let (_dir, store) = store();
    let key = key(FrontEnd::Turbo, b"hash");
    let path = store.entry_path(&key);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"garbage").unwrap();

    assert!(matches!(store.get(&key), Err(Error::MalformedEntry { .. })));
}

#[test]
fn writes_leave_no_pending_files() {
    let (_dir, store) = store();
    let blob = store.ingest(&b"x"[..], None).unwrap().digest;
    store
        .put(&key(FrontEnd::Reapi, b"k"), &Entry::new(vec![blob]))
        .unwrap();

    assert_eq!(fs::read_dir(store.root().join("pending")).unwrap().count(), 0);
}

#[test]
fn entries_and_index_files_are_sharded_by_their_first_byte() {
    let (_dir, store) = store();
    let key = key(FrontEnd::Turbo, b"hash");
    let hex = key.to_string();

    assert_eq!(
        store.entry_path(&key),
        store.root().join("ac").join(&hex[..2]).join(&hex[2..])
    );
}
