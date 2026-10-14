//! Unit tests for the command-line error type.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn store_location_error_tells_the_user_how_to_fix_it() {
    assert_eq!(
        Error::NoStoreLocation.to_string(),
        "no store location: pass --store or set TINYMOLD_STORE"
    );
}

#[test]
fn library_errors_convert_and_render_transparently() {
    let hash = tinymold_hash::Error::InvalidHex { text: "x".into() };
    let expected = hash.to_string();
    assert_eq!(Error::from(hash).to_string(), expected);

    let cas = tinymold_cas::Error::DestinationExists("out".into());
    assert_eq!(
        Error::from(cas).to_string(),
        "destination out already exists"
    );
}

#[test]
fn output_errors_keep_their_cause() {
    let error = Error::Output(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
    assert_eq!(error.to_string(), "failed to write output");
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn freshness_and_snapshot_errors_say_what_went_wrong() {
    let io = Error::Io {
        action: "read",
        path: "target".into(),
        source: std::io::Error::from(std::io::ErrorKind::NotFound),
    };
    assert_eq!(io.to_string(), "failed to read target");
    assert!(std::error::Error::source(&io).is_some());

    let cases = [
        (
            Error::CorruptManifest {
                path: "fresh".into(),
                line: 3,
            },
            "freshness manifest fresh is corrupt at line 3",
        ),
        (Error::NotADirectory("f".into()), "f is not a directory"),
        (Error::AlreadyExists("t".into()), "t already exists"),
        (
            Error::InvalidSnapshotName("a/b".into()),
            "invalid snapshot name \"a/b\": use ASCII letters, digits, '.', '_' and '-'",
        ),
        (Error::SnapshotNotFound("x".into()), "no snapshot named x"),
        (
            Error::SnapshotUnusable("x".into()),
            "snapshot x has no valid freshness manifest and was not restored",
        ),
        (
            Error::NoSnapshots,
            "the store holds no snapshots: run tinymold snapshot save after a build",
        ),
        (
            Error::NoSnapshotName,
            "cannot name the snapshot from git HEAD: pass a name",
        ),
    ];
    for (error, message) in cases {
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn invalid_limits_say_what_is_accepted() {
    assert_eq!(
        Error::InvalidDuration("2w".into()).to_string(),
        "invalid duration \"2w\": use a whole number and s, m, h or d, such as 14d"
    );
    assert_eq!(
        Error::InvalidSize("lots".into()).to_string(),
        "invalid size \"lots\": use a whole number of bytes, or K, M, G or T, such as 10G"
    );
}

#[test]
fn wrapper_errors_convert_transparently() {
    let wrap = tinymold_wrap::Error::CorruptRecord {
        path: "entry".into(),
        line: 3,
    };
    let expected = wrap.to_string();
    assert_eq!(Error::from(wrap).to_string(), expected);
}

#[test]
fn a_missing_log_location_says_how_to_give_one() {
    assert_eq!(
        Error::NoLogLocation.to_string(),
        "no wrapper log: pass --log or set TINYMOLD_LOG"
    );
}
