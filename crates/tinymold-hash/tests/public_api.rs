//! Integration tests for the public `tinymold-hash` surface.
//!
//! These link against the crate as a downstream consumer would, so they pin
//! the contract other tinymold crates rely on.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use tinymold_hash::{Digest, Error, Hasher};

#[test]
fn digests_are_usable_as_map_keys_and_sort_stably() {
    let mut digests = [Digest::of_bytes(b"b"), Digest::of_bytes(b"a")];
    digests.sort();
    let set: std::collections::BTreeSet<_> = digests.iter().copied().collect();

    assert_eq!(set.len(), 2);
}

#[test]
fn hasher_and_parse_errors_are_public() {
    let digest = Hasher::new().field(b"key").finish();

    assert_eq!(digest.to_hex().len(), 64);
    assert!(matches!(
        "".parse::<Digest>(),
        Err(Error::InvalidHex { .. })
    ));
}
