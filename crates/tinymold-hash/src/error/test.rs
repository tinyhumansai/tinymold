//! Unit tests for the hashing error type.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn a_read_failure_names_the_path_and_keeps_its_cause() {
    let error = Error::Read {
        path: PathBuf::from("src/lib.rs"),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "gone"),
    };

    assert_eq!(error.to_string(), "failed to read src/lib.rs");
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn invalid_hex_quotes_the_rejected_text() {
    let error = Error::InvalidHex {
        text: "xyz".to_owned(),
    };

    assert_eq!(
        error.to_string(),
        "invalid digest \"xyz\": expected 64 hexadecimal characters"
    );
}
