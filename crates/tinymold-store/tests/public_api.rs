//! Integration tests for the public `tinymold-store` surface.
//!
//! They use the store the way the server's front-ends will: a blob arrives
//! through one protocol, an entry names it, and another protocol finds the
//! same blob by the digest it speaks.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use tinymold_store::{Claim, Entry, Error, FrontEnd, Namespace, Sha256, Store};

#[test]
fn a_blob_uploaded_through_one_protocol_is_found_by_another_and_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();

    // Turbo uploads an artifact under its task hash.
    let artifact = store.ingest(&b"turbo task archive"[..], None).unwrap();
    let turbo = Namespace::new(FrontEnd::Turbo, "team").unwrap();
    store
        .put(
            &turbo.key(b"0123456789abcdef").unwrap(),
            &Entry::new(vec![artifact.digest]),
        )
        .unwrap();

    // A REAPI client asks for the same bytes by SHA-256, then uploads them
    // again with a correct claim.
    let sha256 = Sha256::of_bytes(b"turbo task archive");
    assert_eq!(store.find_sha256(&sha256).unwrap(), Some(artifact.digest));
    store
        .ingest(&b"turbo task archive"[..], Some(Claim::Sha256(sha256)))
        .unwrap();

    assert_eq!(store.cas().objects().unwrap().len(), 1);
}

#[test]
fn a_key_written_through_one_front_end_misses_through_another() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    let blob = store.ingest(&b"output"[..], None).unwrap().digest;

    let nx = Namespace::new(FrontEnd::Nx, "ci")
        .unwrap()
        .key(b"h")
        .unwrap();
    let turbo = Namespace::new(FrontEnd::Turbo, "ci")
        .unwrap()
        .key(b"h")
        .unwrap();
    store.put(&nx, &Entry::new(vec![blob])).unwrap();

    assert!(store.get(&nx).unwrap().is_some());
    assert!(store.get(&turbo).unwrap().is_none());
}

#[test]
fn a_reopened_store_serves_what_it_held() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let key = Namespace::new(FrontEnd::Gradle, "")
        .unwrap()
        .key(b"task")
        .unwrap();
    {
        let store = Store::open(&root).unwrap();
        let blob = store.ingest(&b"cache entry"[..], None).unwrap().digest;
        store.put(&key, &Entry::new(vec![blob])).unwrap();
    }

    let store = Store::open(&root).unwrap();
    let entry = store.get(&key).unwrap().unwrap();
    assert_eq!(store.cas().read(&entry.blobs[0]).unwrap(), b"cache entry");
}

#[test]
fn a_wrong_claim_is_an_error_the_caller_can_match() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    let claim = Claim::Sha256(Sha256::of_bytes(b"something else"));

    assert!(matches!(
        store.ingest(&b"bytes"[..], Some(claim)),
        Err(Error::DigestMismatch { .. })
    ));
}
