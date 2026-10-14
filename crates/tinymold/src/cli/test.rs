//! Unit tests for argument parsing and command dispatch.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::io;

use super::*;

/// Parses and runs a command line, returning the outcome and stdout.
fn exec(args: &[&str]) -> (Result<Outcome>, String) {
    let cli = Cli::try_parse_from(std::iter::once("tinymold").chain(args.iter().copied())).unwrap();
    let mut out = Vec::new();
    let result = run(&cli, &mut out);
    (result, String::from_utf8(out).unwrap())
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn hash_prints_digest_and_path_like_b3sum() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.rs");
    fs::write(&file, b"fn main() {}").unwrap();

    let (result, out) = exec(&["hash", s(&file)]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(
        out,
        format!(
            "{}  {}\n",
            Digest::of_bytes(b"fn main() {}"),
            file.display()
        )
    );
}

#[test]
fn hash_fails_on_a_missing_file() {
    let (result, _) = exec(&["hash", "/nonexistent/file.rs"]);
    assert!(matches!(result, Err(Error::Hash(_))));
}

#[test]
fn put_get_has_and_verify_round_trip_through_a_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let file = dir.path().join("libdep.rlib");
    fs::write(&file, b"rlib bytes").unwrap();
    let digest = Digest::of_bytes(b"rlib bytes").to_hex();

    let (result, out) = exec(&["--store", s(&store), "cas", "put", s(&file)]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(out.starts_with(&digest));

    let (result, _) = exec(&["--store", s(&store), "cas", "has", &digest]);
    assert_eq!(result.unwrap(), Outcome::Success);

    let (result, out) = exec(&["--store", s(&store), "cas", "verify", &digest]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(out, format!("ok  {digest}\n"));

    let restored = dir.path().join("target/libdep.rlib");
    let (result, out) = exec(&[
        "--store",
        s(&store),
        "cas",
        "get",
        "--strategy",
        "copy",
        &digest,
        s(&restored),
    ]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(out, format!("copy  {}\n", restored.display()));
    assert_eq!(fs::read(&restored).unwrap(), b"rlib bytes");
}

#[test]
fn has_reports_absence_through_the_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let digest = Digest::of_bytes(b"absent").to_hex();

    let (result, out) = exec(&["--store", s(dir.path()), "cas", "has", &digest]);
    assert_eq!(result.unwrap(), Outcome::Absent);
    assert_eq!(out, "");
}

#[test]
fn store_errors_propagate() {
    let dir = tempfile::tempdir().unwrap();
    let digest = Digest::of_bytes(b"absent").to_hex();

    let (result, _) = exec(&["--store", s(dir.path()), "cas", "verify", &digest]);
    assert!(matches!(
        result,
        Err(Error::Cas(tinymold_cas::Error::NotFound(_)))
    ));
}

#[test]
fn doctor_prints_a_report() {
    let dir = tempfile::tempdir().unwrap();
    let (result, out) = exec(&["--store", s(dir.path()), "doctor"]);

    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(out.starts_with("tinymold "));
    assert!(out.contains(&dir.path().display().to_string()));
}

#[test]
fn rejects_a_malformed_digest_at_parse_time() {
    let error = Cli::try_parse_from(["tinymold", "cas", "has", "not-a-digest"]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn every_strategy_maps_to_the_store_strategy() {
    let pairs = [
        (StrategyArg::Auto, Strategy::Auto),
        (StrategyArg::Reflink, Strategy::Reflink),
        (StrategyArg::Hardlink, Strategy::Hardlink),
        (StrategyArg::Copy, Strategy::Copy),
    ];
    for (arg, strategy) in pairs {
        assert_eq!(Strategy::from(arg), strategy);
    }
}

#[test]
fn outcomes_map_to_exit_codes() {
    assert_eq!(ExitCode::from(Outcome::Success), ExitCode::SUCCESS);
    assert_eq!(ExitCode::from(Outcome::Absent), ExitCode::from(1));
}

/// A writer whose every write fails, standing in for a closed stdout.
struct Closed;

impl Write for Closed {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn output_failures_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a");
    fs::write(&file, b"a").unwrap();
    let cli = Cli::try_parse_from(["tinymold", "hash", s(&file)]).unwrap();

    assert!(matches!(run(&cli, &mut Closed), Err(Error::Output(_))));

    let cli = Cli::try_parse_from(["tinymold", "--store", s(dir.path()), "doctor"]).unwrap();
    assert!(matches!(run(&cli, &mut Closed), Err(Error::Output(_))));
}

#[test]
fn a_store_path_that_is_a_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, b"").unwrap();
    let digest = Digest::of_bytes(b"").to_hex();

    let (result, _) = exec(&["--store", s(&file), "cas", "has", &digest]);
    assert!(matches!(result, Err(Error::Cas(_))));
}
