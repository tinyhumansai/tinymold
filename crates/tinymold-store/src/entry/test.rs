//! Unit tests for entry records and their encoding.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn sample() -> Entry {
    let mut entry = Entry::new(vec![Digest::of_bytes(b"one"), Digest::of_bytes(b"two")]);
    entry.body = b"action result".to_vec();
    entry
        .meta
        .insert("x-artifact-duration".into(), "1200".into());
    entry.meta.insert("x-artifact-tag".into(), "sig".into());
    entry.written = 1_760_000_000;
    entry
}

#[test]
fn an_entry_round_trips() {
    let entry = sample();

    assert_eq!(Entry::decode(&entry.encode().unwrap()).unwrap(), entry);
}

#[test]
fn the_entry_encoding_is_pinned() {
    let mut entry = Entry::new(vec![Digest::of_bytes(b"")]);
    entry.body = b"b".to_vec();
    entry.meta.insert("k".into(), "v".into());
    entry.written = 1;
    let encoded = entry.encode().unwrap();

    let mut expected = b"tmentry1".to_vec();
    expected.extend(1_u64.to_le_bytes());
    expected.extend(1_u32.to_le_bytes());
    expected.extend(Digest::of_bytes(b"").as_bytes());
    expected.extend(1_u64.to_le_bytes());
    expected.extend(b"b");
    expected.extend(1_u32.to_le_bytes());
    expected.extend(1_u32.to_le_bytes());
    expected.extend(b"k");
    expected.extend(1_u32.to_le_bytes());
    expected.extend(b"v");
    assert_eq!(encoded, expected);
}

#[test]
fn an_empty_entry_round_trips() {
    let entry = Entry::new(Vec::new());

    assert_eq!(Entry::decode(&entry.encode().unwrap()).unwrap(), entry);
}

#[test]
fn metadata_over_the_limit_is_refused() {
    let mut entry = Entry::new(Vec::new());
    entry.meta.insert("k".into(), "v".repeat(MAX_META));
    let error = entry.encode().unwrap_err();

    assert!(matches!(error, Error::MetaTooLarge { size } if size == MAX_META + 1));
}

#[test]
fn a_record_without_the_magic_is_malformed() {
    let error = Entry::decode(b"not an entry record").unwrap_err();

    assert!(matches!(
        error,
        Error::MalformedEntry {
            reason: "unknown format"
        }
    ));
}

#[test]
fn a_truncated_record_is_malformed() {
    let encoded = sample().encode().unwrap();
    let error = Entry::decode(&encoded[..encoded.len() - 1]).unwrap_err();

    assert!(matches!(
        error,
        Error::MalformedEntry {
            reason: "truncated"
        }
    ));
}

#[test]
fn trailing_bytes_are_malformed() {
    let mut encoded = sample().encode().unwrap();
    encoded.push(0);
    let error = Entry::decode(&encoded).unwrap_err();

    assert!(matches!(
        error,
        Error::MalformedEntry {
            reason: "trailing bytes"
        }
    ));
}

#[test]
fn metadata_keys_must_be_in_order() {
    let mut encoded = b"tmentry1".to_vec();
    encoded.extend(0_u64.to_le_bytes());
    encoded.extend(0_u32.to_le_bytes());
    encoded.extend(0_u64.to_le_bytes());
    encoded.extend(2_u32.to_le_bytes());
    for key in [b"b", b"a"] {
        encoded.extend(1_u32.to_le_bytes());
        encoded.extend(key);
        encoded.extend(0_u32.to_le_bytes());
    }
    let error = Entry::decode(&encoded).unwrap_err();

    assert!(matches!(
        error,
        Error::MalformedEntry {
            reason: "metadata out of order"
        }
    ));
}

#[test]
fn metadata_must_be_utf8() {
    let mut encoded = b"tmentry1".to_vec();
    encoded.extend(0_u64.to_le_bytes());
    encoded.extend(0_u32.to_le_bytes());
    encoded.extend(0_u64.to_le_bytes());
    encoded.extend(1_u32.to_le_bytes());
    encoded.extend(1_u32.to_le_bytes());
    encoded.push(0xff);
    encoded.extend(0_u32.to_le_bytes());
    let error = Entry::decode(&encoded).unwrap_err();

    assert!(matches!(
        error,
        Error::MalformedEntry {
            reason: "metadata is not utf-8"
        }
    ));
}
