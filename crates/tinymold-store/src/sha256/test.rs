//! Unit tests for SHA-256 digests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn the_empty_input_matches_the_standard_vector() {
    assert_eq!(
        Sha256::of_bytes(b"").to_hex(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn hex_round_trips_in_either_case() {
    let digest = Sha256::of_bytes(b"abc");
    let upper = digest.to_hex().to_uppercase();

    assert_eq!(upper.parse::<Sha256>().unwrap(), digest);
    assert_eq!(digest.to_string(), digest.to_hex());
}

#[test]
fn malformed_hex_is_refused() {
    for text in ["", "abc", &"g".repeat(64), &"a".repeat(65)] {
        let error = text.parse::<Sha256>().unwrap_err();
        assert!(matches!(error, Error::InvalidSha256(ref got) if got == text));
    }
}
