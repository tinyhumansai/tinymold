//! Unit tests for file identities, the stat cache, and compiler identities.

#![allow(clippy::unwrap_used)]

use std::time::{Duration, SystemTime};

use super::*;

/// The store's filesystem clock, as the wrapper reads it once per call.
fn now(store: &Path) -> u64 {
    crate::clock::filesystem_now(&store.join("stat")).unwrap()
}

#[test]
fn a_digest_is_reused_until_the_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let file = dir.path().join("libdep.rlib");
    fs::write(&file, b"one").unwrap();
    let old = SystemTime::now() - Duration::from_secs(60);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();

    assert_eq!(digest(&store, &file, now(&store)).unwrap(), Digest::of_bytes(b"one"));
    // A wrong cached digest is returned as-is, which proves the cache is read.
    let poisoned = Digest::of_bytes(b"cached");
    remember(&store, &file, identity(&file).unwrap(), poisoned).unwrap();
    let expected = if cfg!(unix) {
        poisoned
    } else {
        Digest::of_bytes(b"one")
    };
    assert_eq!(digest(&store, &file, now(&store)).unwrap(), expected);

    // Same size, new content and mtime: the entry no longer applies.
    fs::write(&file, b"two").unwrap();
    assert_eq!(digest(&store, &file, now(&store)).unwrap(), Digest::of_bytes(b"two"));
    assert!(digest(&store, &dir.path().join("missing"), now(&store)).is_err());
}

#[test]
fn validity_needs_an_inode_an_equal_identity_and_an_older_mtime() {
    let id = Identity {
        dev: 1,
        ino: 2,
        size: 3,
        mtime_ns: 1_234_567_891,
    };
    assert!(still_valid(id, 1_234_567_892, id));
    assert!(!still_valid(id, 1_234_567_891, id), "racily clean");
    assert!(!still_valid(id, 1_234_567_892, Identity { size: 4, ..id }));
    let no_inode = Identity { ino: 0, ..id };
    assert!(!still_valid(no_inode, 1_234_567_892, no_inode));
}

#[test]
fn a_whole_second_mtime_is_not_trusted_within_its_tick() {
    // HFS+ and ext3 keep whole seconds: a file hashed at 10.5 s and
    // rewritten at 10.8 s, same size, still reads back as 10 s.
    let second = 1_000_000_000;
    let id = Identity {
        dev: 1,
        ino: 2,
        size: 3,
        mtime_ns: 10 * second,
    };
    assert!(!still_valid(id, 10 * second + second / 2, id));
}

#[test]
fn a_corrupt_stat_entry_is_ignored() {
    assert_eq!(parse_entry("garbage"), None);
    assert_eq!(parse_entry("1 2 3 4 5 nothex"), None);
    let d = Digest::of_bytes(b"");
    let (identity, recorded, digest) = parse_entry(&format!("1 2 3 4 5 {d}\n")).unwrap();
    assert_eq!((identity.ino, recorded, digest), (2, 5, d));
}

#[cfg(unix)]
#[test]
fn rustc_version_runs_once_per_compiler_identity() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let counter = dir.path().join("runs");
    let program = dir.path().join("rustc");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\necho run >> {}\necho 'rustc 9.9.9'\n",
            counter.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(rustc_version(&store, &program).unwrap(), "rustc 9.9.9\n");
    assert_eq!(rustc_version(&store, &program).unwrap(), "rustc 9.9.9\n");
    assert_eq!(fs::read_to_string(&counter).unwrap().lines().count(), 1);

    let failing = dir.path().join("broken");
    fs::write(&failing, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&failing, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(rustc_version(&store, &failing).unwrap(), "");
    assert!(rustc_version(&store, &dir.path().join("absent")).is_err());
}

#[test]
fn linker_identity_names_the_file_or_says_it_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("ld"), b"linker").unwrap();
    let found = linker_identity(Path::new("./ld"), dir.path());
    assert!(found.ends_with(&format!(
        " 6 {}",
        identity(&dir.path().join("ld")).unwrap().mtime_ns
    )));
    assert_eq!(
        linker_identity(Path::new("no-such-linker-anywhere"), dir.path()),
        "missing no-such-linker-anywhere"
    );
}

#[test]
fn archives_are_digested_without_their_build_stamps() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let archive = |mtime: &str| {
        let mut bytes = b"!<arch>\n".to_vec();
        let header = format!(
            "{:<16}{mtime:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            "a.o/", 0, 0, 644, 2
        );
        bytes.extend(header.as_bytes());
        bytes.extend(b"ok");
        bytes
    };
    let first = dir.path().join("one.a");
    let second = dir.path().join("two.a");
    fs::write(&first, archive("1700000000")).unwrap();
    fs::write(&second, archive("1800000000")).unwrap();
    let a = digest_as(&store, &first, Kind::Archive, now(&store)).unwrap();
    assert_eq!(a, digest_as(&store, &second, Kind::Archive, now(&store)).unwrap());
    assert_ne!(
        a,
        digest(&store, &first, now(&store)).unwrap(),
        "the byte digest is kept apart"
    );
    assert_eq!(
        a,
        digest_as(&store, &first, Kind::Archive, now(&store)).unwrap(),
        "cached"
    );
}
