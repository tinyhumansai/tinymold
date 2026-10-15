//! Unit tests for the store error type.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::{Claim, Sha256};

#[test]
fn io_errors_say_what_failed_and_where() {
    let error = Error::Io {
        action: "write",
        path: PathBuf::from("/store/ac/ab"),
        source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    };

    assert_eq!(error.to_string(), "failed to write /store/ac/ab");
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn a_digest_mismatch_names_both_digests() {
    let claimed = Claim::Sha256(Sha256::of_bytes(b"claimed"));
    let actual = Claim::Sha256(Sha256::of_bytes(b"actual"));
    let message = Error::DigestMismatch { claimed, actual }.to_string();

    assert!(message.contains(&format!("sha256:{}", Sha256::of_bytes(b"claimed"))));
    assert!(message.contains(&format!("sha256:{}", Sha256::of_bytes(b"actual"))));
}

#[test]
fn a_missing_object_names_it() {
    let digest = Digest::of_bytes(b"gone");

    assert!(Error::MissingObject(digest).to_string().contains(&digest.to_hex()));
}
