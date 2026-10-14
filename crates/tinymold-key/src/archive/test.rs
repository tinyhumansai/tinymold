//! Unit tests for archive digests.

use super::*;

/// One `ar` member with the given name, mtime and data.
fn member(name: &str, mtime: u64, data: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "{name:<16}{mtime:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        501,
        20,
        "100644",
        data.len()
    )
    .into_bytes();
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(b'\n');
    }
    out
}

fn archive(mtime: u64) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend(member("a.o/", mtime, b"odd"));
    out.extend(member("b.o/", mtime, b"even"));
    out
}

#[test]
fn archives_differing_only_in_build_stamps_share_a_digest() {
    assert_eq!(content_digest(&archive(1_700_000_000)), content_digest(&archive(1_800_000_000)));
}

#[test]
fn a_changed_member_changes_the_digest() {
    let mut changed = archive(1);
    let last = changed.len() - 1;
    changed[last] = b'X';
    assert_ne!(content_digest(&changed), content_digest(&archive(1)));
}

#[test]
fn anything_else_is_hashed_as_it_is() {
    assert_eq!(content_digest(b"not an archive"), Digest::of_bytes(b"not an archive"));
    // Truncated, a bad terminator, and a size running past the end.
    let full = archive(1);
    for broken in [&full[..20], &[MAGIC, b"short"].concat()[..]] {
        assert_eq!(content_digest(broken), Digest::of_bytes(broken));
    }
    let mut bad_terminator = archive(1);
    bad_terminator[MAGIC.len() + 58] = b'x';
    assert_eq!(content_digest(&bad_terminator), Digest::of_bytes(&bad_terminator));
    let mut too_long = MAGIC.to_vec();
    too_long.extend(member("a.o/", 1, b"data"));
    too_long.truncate(too_long.len() - 2);
    assert_eq!(content_digest(&too_long), Digest::of_bytes(&too_long));
}
