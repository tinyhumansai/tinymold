//! Integration tests for the public `tinymold-cas` surface.
//!
//! They use the store exactly as the rest of tinymold does: insert an
//! artifact once, restore it into several independent target directories.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;

use tinymold_cas::{Digest, Method, Store, Strategy};

#[test]
fn one_object_serves_many_worktrees() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    let digest = store.insert_bytes(b"shared dependency rlib").unwrap();

    for worktree in ["a", "b", "c"] {
        let destination = dir
            .path()
            .join(worktree)
            .join("target/debug/deps/libdep.rlib");
        let method = store
            .materialize(&digest, &destination, Strategy::Auto)
            .unwrap();
        assert!(matches!(
            method,
            Method::Reflink | Method::Hardlink | Method::Copy
        ));
        assert_eq!(fs::read(&destination).unwrap(), b"shared dependency rlib");
    }

    assert_eq!(digest, Digest::of_bytes(b"shared dependency rlib"));
}

#[test]
fn a_second_handle_on_the_same_root_sees_the_same_objects() {
    let dir = tempfile::tempdir().unwrap();
    let writer = Store::open(dir.path()).unwrap();
    let digest = writer.insert_bytes(b"visible").unwrap();

    let reader = Store::open(dir.path()).unwrap();
    assert!(reader.contains(&digest));
    reader.verify(&digest).unwrap();
}

#[test]
fn a_streamed_upload_is_checked_before_it_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    let claimed = Digest::of_bytes(b"what the client said it sent");

    let staged = store.stage(&b"what actually arrived"[..]).unwrap();
    assert_ne!(staged.digest(), claimed);
    let rejected = staged.digest();
    drop(staged);

    assert!(!store.contains(&rejected));
}
