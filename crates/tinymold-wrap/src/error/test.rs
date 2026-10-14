//! Unit tests for the wrapper's error type.

#![allow(clippy::unwrap_used)]

use super::*;

#[test]
fn errors_say_what_failed_and_keep_their_cause() {
    let io_error = io("read", std::path::Path::new("f"))(std::io::Error::other("x"));
    assert_eq!(io_error.to_string(), "failed to read f");
    assert!(std::error::Error::source(&io_error).is_some());

    let corrupt = Error::CorruptRecord {
        path: "r".into(),
        line: 2,
    };
    assert_eq!(corrupt.to_string(), "cache record r is corrupt at line 2");

    let spawn = Error::Spawn {
        program: "rustc".into(),
        source: std::io::Error::other("x"),
    };
    assert_eq!(spawn.to_string(), "failed to run rustc");

    let key = Error::from(tinymold_key::Error::MalformedDepInfo { line: 1 });
    assert_eq!(key.to_string(), "malformed dep-info at line 1");
    let hash = Error::from(tinymold_hash::Error::InvalidHex { text: "x".into() });
    assert!(!hash.to_string().is_empty());
    let cas = Error::from(tinymold_cas::Error::DestinationExists("d".into()));
    assert_eq!(cas.to_string(), "destination d already exists");
}
