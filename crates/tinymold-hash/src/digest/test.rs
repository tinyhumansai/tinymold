//! Unit tests for digests and the field hasher.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{self, Read, Write};

use super::*;

/// BLAKE3 of the empty input, from the BLAKE3 reference test vectors.
const EMPTY: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

#[test]
fn matches_the_reference_digest_of_the_empty_input() {
    assert_eq!(Digest::of_bytes(b"").to_hex(), EMPTY);
}

#[test]
fn hex_round_trips() {
    let digest = Digest::of_bytes(b"tinymold");
    assert_eq!(digest.to_hex().parse::<Digest>().unwrap(), digest);
}

#[test]
fn parses_uppercase_hex() {
    let parsed: Digest = EMPTY.to_uppercase().parse().unwrap();
    assert_eq!(parsed, Digest::of_bytes(b""));
}

#[test]
fn rejects_hex_of_the_wrong_length() {
    let error = "abcd".parse::<Digest>().unwrap_err();
    assert!(matches!(error, Error::InvalidHex { text } if text == "abcd"));
}

#[test]
fn rejects_non_hex_characters() {
    let text = format!("{}zz", &EMPTY[..62]);
    assert!(matches!(
        text.parse::<Digest>(),
        Err(Error::InvalidHex { .. })
    ));
    let text = format!("{}z0", &EMPTY[..62]);
    assert!(matches!(
        text.parse::<Digest>(),
        Err(Error::InvalidHex { .. })
    ));
}

#[test]
fn raw_bytes_round_trip() {
    let digest = Digest::of_bytes(b"bytes");
    assert_eq!(Digest::from_bytes(*digest.as_bytes()), digest);
}

#[test]
fn debug_shows_the_hex_form() {
    assert_eq!(format!("{:?}", Digest::of_bytes(b"")), format!("Digest({EMPTY})"));
}

#[test]
fn a_reader_and_a_slice_agree() {
    let data = vec![7_u8; 3 * READ_BUFFER + 11];
    assert_eq!(
        Digest::of_reader(data.as_slice()).unwrap(),
        Digest::of_bytes(&data)
    );
}

#[test]
fn a_file_and_a_slice_agree() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"pub fn answer() -> u8 { 42 }").unwrap();

    assert_eq!(
        Digest::of_file(file.path()).unwrap(),
        Digest::of_bytes(b"pub fn answer() -> u8 { 42 }")
    );
}

#[test]
fn a_missing_file_reports_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.rs");

    let error = Digest::of_file(&missing).unwrap_err();
    assert!(matches!(error, Error::Read { path, .. } if path == missing));
}

/// A reader that is interrupted once, then fails, to exercise both branches.
struct Flaky {
    calls: usize,
}

impl Read for Flaky {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        match self.calls {
            1 => Err(io::Error::from(io::ErrorKind::Interrupted)),
            _ => Err(io::Error::other("disk on fire")),
        }
    }
}

#[test]
fn retries_interrupted_reads_and_reports_real_failures() {
    let error = Digest::of_reader(Flaky { calls: 0 }).unwrap_err();
    assert!(matches!(error, Error::Read { path, .. } if path == Path::new("<reader>")));
}

#[test]
fn field_boundaries_change_the_digest() {
    let mut joined = Hasher::new();
    joined.field(b"abc");
    let mut split = Hasher::new();
    split.field(b"a").field(b"bc");

    assert_ne!(joined.finish(), split.finish());
}

#[test]
fn equal_fields_give_equal_digests() {
    let inner = Digest::of_bytes(b"dep.rmeta");
    let build = || {
        let mut hasher = Hasher::new();
        hasher.field(b"tinymold/rustc/v1").digest(&inner);
        hasher.finish()
    };

    assert_eq!(build(), build());
}
