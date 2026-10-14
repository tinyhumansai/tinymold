//! End-to-end tests that run the real `tinymold` binary.
//!
//! The library tests cover every command; these pin what only the binary
//! does: exit codes and the error report on stderr.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

fn tinymold() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tinymold"))
}

#[test]
fn hash_succeeds_and_prints_one_line_per_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, b"").unwrap();

    let output = tinymold().arg("hash").arg(&file).output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 1);
}

#[test]
fn has_exits_one_for_an_absent_object() {
    let dir = tempfile::tempdir().unwrap();
    let output = tinymold()
        .args(["--store"])
        .arg(dir.path())
        .args(["cas", "has", &"0".repeat(64)])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn failures_print_the_cause_chain_and_exit_nonzero() {
    let output = tinymold()
        .args(["hash", "/nonexistent/input.rs"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("tinymold: failed to read /nonexistent/input.rs"));
    assert!(stderr.contains("caused by:"));
}
