//! Unit tests for the action and pre-key record encodings.

#![allow(clippy::unwrap_used)]

use super::*;

fn digest(text: &str) -> Digest {
    Digest::of_bytes(text.as_bytes())
}

#[test]
fn the_action_encoding_is_pinned() {
    let action = Action {
        stdout: Digest::of_bytes(b""),
        stderr: Digest::of_bytes(b""),
        outputs: vec![Output {
            name: "libdemo-1.rlib".into(),
            digest: Digest::of_bytes(b""),
            executable: false,
            text: true,
        }],
    };
    let empty = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    let expected = format!(
        "tinymold-action 1\nstdout {empty}\nstderr {empty}\noutput {empty} 0 1 libdemo-1.rlib\n"
    );
    assert_eq!(action.encode(), expected);
    assert_eq!(Action::decode(&expected, Path::new("a")).unwrap(), action);
}

#[test]
fn the_pre_key_record_encoding_is_pinned() {
    let sets = vec![
        InputSet {
            recorded_ns: 9,
            inputs: vec![Input {
                path: "src/my file.rs".into(),
                identity: Identity {
                    dev: 1,
                    ino: 2,
                    size: 3,
                    mtime_ns: 4,
                },
                digest: Digest::of_bytes(b""),
            }],
            env: vec!["CARGO_PKG_VERSION".into()],
        },
        InputSet {
            recorded_ns: 5,
            inputs: vec![],
            env: vec![],
        },
    ];
    let expected = "tinymold-prekey 1\nset 9\n\
        input 1 2 3 4 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 src/my file.rs\n\
        env CARGO_PKG_VERSION\nset 5\n";
    assert_eq!(encode_sets(&sets), expected);
    assert_eq!(decode_sets(expected, Path::new("p")).unwrap(), sets);
}

#[test]
fn corrupt_records_name_the_bad_line() {
    let d = digest("x");
    let action_cases = [
        (String::new(), 1),
        ("tinymold-action 1\nstdout nothex\n".to_owned(), 2),
        (format!("tinymold-action 1\nstdout {d}\n"), 3),
        (format!("tinymold-action 1\nstdout {d}\nstderr {d}\noutput {d} 2 0 x\n"), 4),
        (format!("tinymold-action 1\nstdout {d}\nstderr {d}\noutput {d} 0 0 \n"), 4),
        (format!("tinymold-action 1\nstdout {d}\nstderr {d}\nnonsense\n"), 4),
    ];
    for (text, line) in action_cases {
        let error = Action::decode(&text, Path::new("a")).unwrap_err();
        assert!(
            matches!(error, Error::CorruptRecord { line: l, .. } if l == line),
            "{text:?}"
        );
    }
    let prekey_cases = [
        (String::new(), 1),
        ("tinymold-prekey 1\ninput 1 2 3 4 x y\n".to_owned(), 2),
        ("tinymold-prekey 1\nset x\n".to_owned(), 2),
        ("tinymold-prekey 1\nset 1\ninput 1 2 3 4 nothex p\n".to_owned(), 3),
        ("tinymold-prekey 1\nset 1\nenv \n".to_owned(), 3),
        ("tinymold-prekey 1\nset 1\nother\n".to_owned(), 3),
    ];
    for (text, line) in prekey_cases {
        let error = decode_sets(&text, Path::new("p")).unwrap_err();
        assert!(
            matches!(error, Error::CorruptRecord { line: l, .. } if l == line),
            "{text:?}"
        );
    }
}

#[test]
fn records_are_written_whole_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/record");
    assert_eq!(read(&path).unwrap(), None);
    write_atomically(&path, "text").unwrap();
    assert_eq!(read(&path).unwrap().as_deref(), Some("text"));
    assert!(matches!(read(dir.path()), Err(Error::Io { action: "read", .. })));

    let blocker = dir.path().join("file");
    std::fs::write(&blocker, b"").unwrap();
    let error = write_atomically(&blocker.join("record"), "x").unwrap_err();
    assert!(matches!(error, Error::Io { action: "create", .. }));
}
