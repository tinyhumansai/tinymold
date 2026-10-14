//! Pins the names tinymold uses on a shared REAPI server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn blobs_are_named_by_sha256_and_size() {
    let empty = blob(b"");
    assert_eq!(
        empty.hash,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(empty.size_bytes, 0);
    let abc = blob(b"abc");
    assert_eq!(
        abc.hash,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(abc.size_bytes, 3);
}

#[test]
fn action_keys_are_the_full_key_and_pre_keys_are_salted() {
    let key = Digest::of_bytes(b"key");
    assert_eq!(action_key(&key).hash, key.to_hex());
    assert_eq!(action_key(&key).size_bytes, 0);
    assert_ne!(prekey_key(&key).hash, key.to_hex());
    assert_eq!(
        prekey_key(&Digest::of_bytes(b"")).hash,
        "edbe34fe0ad0d18146fc2f457a9635218f92350222ceeaedf1849a12370ab02c"
    );
}

#[test]
fn resources_follow_the_reapi_grammar() {
    let digest = blob(b"abc");
    assert_eq!(
        read_resource("", &digest),
        format!("blobs/{}/3", digest.hash)
    );
    assert_eq!(
        read_resource("main", &digest),
        format!("main/blobs/{}/3", digest.hash)
    );
    let write = write_resource("main", &digest);
    let parts: Vec<_> = write.split('/').collect();
    assert_eq!(parts[0], "main");
    assert_eq!(parts[1], "uploads");
    assert_eq!(parts[2].len(), 36);
    assert_eq!(&parts[3..], ["blobs", digest.hash.as_str(), "3"]);
    assert_ne!(write, write_resource("main", &digest));
}
