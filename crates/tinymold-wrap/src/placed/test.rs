//! Unit tests for the records of outputs placed in an out-dir.

#![allow(clippy::unwrap_used)]

use std::fs;

use super::*;

fn identity(ino: u64, mtime_ns: u64) -> Identity {
    Identity {
        dev: 1,
        ino,
        size: 10,
        mtime_ns,
    }
}

#[test]
fn the_record_format_is_pinned() {
    let placed = Placed {
        files: HashMap::from([
            (
                "libdemo-1.rmeta".to_owned(),
                (identity(8, 5), Digest::of_bytes(b"m")),
            ),
            (
                "demo-1.a.rcgu.o".to_owned(),
                (identity(7, 4), Digest::of_bytes(b"o")),
            ),
        ]),
    };
    let text = placed.encode();
    assert_eq!(
        text,
        format!(
            "tinymold-placed 1\n1 7 10 4 {} demo-1.a.rcgu.o\n1 8 10 5 {} libdemo-1.rmeta\n",
            Digest::of_bytes(b"o"),
            Digest::of_bytes(b"m")
        )
    );
    assert_eq!(Placed::decode(&text).unwrap(), placed);
}

#[test]
fn a_file_is_held_only_with_its_identity_and_digest_unchanged() {
    let digest = Digest::of_bytes(b"o");
    let placed = Placed {
        files: HashMap::from([
            ("demo-1.o".to_owned(), (identity(7, 50), digest)),
            ("demo-1.x".to_owned(), (identity(0, 50), digest)),
        ]),
    };
    assert!(placed.holds("demo-1.o", identity(7, 50), digest));
    assert!(!placed.holds("demo-1.o", identity(7, 50), Digest::of_bytes(b"other")));
    assert!(!placed.holds("demo-1.o", identity(9, 50), digest));
    assert!(!placed.holds("demo-1.o", identity(7, 51), digest));
    assert!(!placed.holds("other-1.o", identity(7, 50), digest));
    // Without an inode number, nothing is trusted.
    assert!(!placed.holds("demo-1.x", identity(0, 50), digest));
}

#[test]
fn a_saved_record_loads_back_and_a_bad_one_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = Placed::path(dir.path(), Path::new("/w/target/debug/deps"), "demo-1");
    assert!(path.starts_with(dir.path().join("stat")));
    assert_ne!(
        path,
        Placed::path(dir.path(), Path::new("/v/target/debug/deps"), "demo-1")
    );
    assert_eq!(Placed::load(&path).unwrap(), Placed::default());

    let digest = Digest::of_bytes(b"o");
    Placed::save(&path, &[("demo-1.o".to_owned(), identity(7, 3), digest)]).unwrap();
    assert!(
        Placed::load(&path)
            .unwrap()
            .holds("demo-1.o", identity(7, 3), digest)
    );

    for bad in [
        String::new(),
        "tinymold-placed 2\n".to_owned(),
        "tinymold-placed 1\n1 2 3 4 notadigest name\n".to_owned(),
        "tinymold-placed 1\n1 2 3\n".to_owned(),
        format!("tinymold-placed 1\n1 2 3 4 {digest} \n"),
    ] {
        fs::write(&path, &bad).unwrap();
        assert_eq!(Placed::load(&path).unwrap(), Placed::default(), "{bad:?}");
    }

    fs::remove_file(&path).unwrap();
    fs::create_dir_all(&path).unwrap();
    assert!(Placed::load(&path).is_err());
}
