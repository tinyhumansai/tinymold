//! Unit tests for the store error type.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn io_errors_say_what_failed_and_where() {
    let error = Error::Io {
        action: "create",
        path: PathBuf::from("/store/objects"),
        source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    };

    assert_eq!(error.to_string(), "failed to create /store/objects");
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn corruption_names_both_digests() {
    let expected = Digest::of_bytes(b"expected");
    let actual = Digest::of_bytes(b"actual");
    let message = Error::Corrupt { expected, actual }.to_string();

    assert!(message.contains(&expected.to_hex()));
    assert!(message.contains(&actual.to_hex()));
}

#[test]
fn unsupported_names_the_method() {
    let error = Error::Unsupported {
        method: Method::Reflink,
        path: PathBuf::from("out.rlib"),
        source: std::io::Error::from(std::io::ErrorKind::Unsupported),
    };

    assert_eq!(error.to_string(), "cannot materialize out.rlib by reflink");
}

#[test]
fn not_found_and_existing_destinations_render() {
    let digest = Digest::of_bytes(b"");
    assert_eq!(
        Error::NotFound(digest).to_string(),
        format!("object {digest} is not in the store")
    );
    assert_eq!(
        Error::DestinationExists(PathBuf::from("a")).to_string(),
        "destination a already exists"
    );
}

#[test]
fn stream_errors_keep_the_failure_as_their_source() {
    let error = Error::Stream(std::io::Error::other("connection reset"));

    assert_eq!(error.to_string(), "failed to read the stream being stored");
    assert!(std::error::Error::source(&error).is_some());
}
