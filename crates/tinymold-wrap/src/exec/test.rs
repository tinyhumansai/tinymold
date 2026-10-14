//! Unit tests for running programs.

#![allow(clippy::unwrap_used)]

use super::*;

#[cfg(unix)]
#[test]
fn a_captured_run_keeps_both_streams_and_the_exit_code() {
    let args: Vec<OsString> = vec!["-c".into(), "echo out; echo err >&2; exit 3".into()];
    let captured = run_captured(Path::new("sh"), &args).unwrap();
    assert_eq!(captured.code, 3);
    assert_eq!(captured.stdout, b"out\n");
    assert_eq!(captured.stderr, b"err\n");
}

#[cfg(unix)]
#[test]
fn a_pass_through_returns_the_exit_code() {
    let args: Vec<OsString> = vec!["-c".into(), "exit 7".into()];
    assert_eq!(pass_through(Path::new("sh"), &args).unwrap(), 7);
}

#[test]
fn a_missing_program_is_a_spawn_error() {
    let missing = Path::new("/no/such/program");
    assert!(matches!(
        pass_through(missing, &[]),
        Err(Error::Spawn { .. })
    ));
    assert!(matches!(
        run_captured(missing, &[]),
        Err(Error::Spawn { .. })
    ));
}
