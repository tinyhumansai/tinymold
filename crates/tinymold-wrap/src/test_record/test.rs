//! Unit tests for the test-binary and test-run record encodings.

#![allow(clippy::unwrap_used)]

use super::*;

#[test]
fn the_test_binary_encoding_is_pinned() {
    let binary = TestBinary {
        crate_name: "demo".into(),
        dylibs: vec!["libplugin-1.dylib".into()],
    };
    let expected = "tinymold-test-binary 1\ncrate demo\ndylib libplugin-1.dylib\n";
    assert_eq!(binary.encode(), expected);
    assert_eq!(TestBinary::decode(expected, Path::new("b")).unwrap(), binary);
}

#[test]
fn the_test_run_encoding_is_pinned() {
    let run = TestRun {
        stdout: Digest::of_bytes(b""),
        stderr: Digest::of_bytes(b""),
    };
    let empty = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    let expected = format!("tinymold-test-run 1\nstdout {empty}\nstderr {empty}\n");
    assert_eq!(run.encode(), expected);
    assert_eq!(TestRun::decode(&expected, Path::new("r")).unwrap(), run);
    assert_eq!(run.references(), vec![run.stdout, run.stderr]);
}

#[test]
fn corrupt_test_records_name_the_bad_line() {
    let line_of = |error: Error| match error {
        Error::CorruptRecord { line, .. } => line,
        other => panic!("{other}"),
    };
    let path = Path::new("r");
    assert_eq!(line_of(TestRun::decode("", path).unwrap_err()), 1);
    assert_eq!(
        line_of(TestRun::decode("tinymold-test-run 1\nstdout x\n", path).unwrap_err()),
        2
    );
    let empty = Digest::of_bytes(b"");
    let no_stderr = format!("tinymold-test-run 1\nstdout {empty}\n");
    assert_eq!(line_of(TestRun::decode(&no_stderr, path).unwrap_err()), 3);
    let extra = format!("tinymold-test-run 1\nstdout {empty}\nstderr {empty}\nmore\n");
    assert_eq!(line_of(TestRun::decode(&extra, path).unwrap_err()), 4);

    assert_eq!(line_of(TestBinary::decode("nope\n", path).unwrap_err()), 1);
    assert_eq!(
        line_of(TestBinary::decode("tinymold-test-binary 1\n", path).unwrap_err()),
        2
    );
    let odd = "tinymold-test-binary 1\ncrate demo\nwhat\n";
    assert_eq!(line_of(TestBinary::decode(odd, path).unwrap_err()), 3);
}

#[test]
fn a_dylib_outside_the_binary_directory_is_corrupt() {
    // The runner joins each name to the binary's directory, so a record must
    // not be able to name a file anywhere else.
    for name in ["../libx.so", "/lib/libx.so", "sub/libx.so", ""] {
        let text = format!("tinymold-test-binary 1\ncrate demo\ndylib {name}\n");
        assert!(TestBinary::decode(&text, Path::new("b")).is_err(), "{name}");
    }
}

#[test]
fn records_live_under_tests() {
    let store = Path::new("/s");
    let digest = Digest::of_bytes(b"x");
    assert_eq!(
        TestBinary::path(store, &digest),
        store.join("tests").join("bin").join(digest.to_hex())
    );
    assert_eq!(
        TestRun::path(store, &digest),
        store.join("tests").join("run").join(digest.to_hex())
    );
}
