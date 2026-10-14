//! End-to-end tests of `tinymold run-test` as cargo's runner, with
//! `RUSTC_WRAPPER=tinymold`: which test binaries `cargo test` runs again
//! after an edit, and which it replays.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

/// Writes `text` to `path`, dated in the past so a compile reading it may be
/// stored.
fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(10))
        .unwrap();
}

/// A workspace of three crates: `beta` depends on `alpha`, `gamma` on
/// neither. Each has a unit test.
fn workspace(root: &Path) {
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"alpha\", \"beta\", \"gamma\"]\nresolver = \"2\"\n",
    );
    let package = |name: &str, deps: &str| {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}"
        )
    };
    write(&root.join("alpha/Cargo.toml"), &package("alpha", ""));
    write(&root.join("alpha/src/lib.rs"), &alpha_source("1"));
    write(
        &root.join("beta/Cargo.toml"),
        &package("beta", "alpha = { path = \"../alpha\" }\n"),
    );
    write(
        &root.join("beta/src/lib.rs"),
        "pub fn twice() -> u32 { alpha::one() * 2 }\n#[test]\nfn doubles() { assert_eq!(twice(), alpha::one() * 2); }\n",
    );
    write(&root.join("gamma/Cargo.toml"), &package("gamma", ""));
    write(
        &root.join("gamma/src/lib.rs"),
        "pub fn three() -> u32 { 3 }\n#[test]\nfn is_three() { assert_eq!(three(), 3); }\n",
    );
}

fn alpha_source(one: &str) -> String {
    format!("pub fn one() -> u32 {{ {one} }}\n#[test]\nfn is_one() {{ assert_eq!(one(), 1); }}\n")
}

/// Runs `cargo test --lib` in `root` with tinymold as wrapper and runner,
/// and returns cargo's output and the runner's lines of the log.
fn cargo_test(root: &Path, store: &Path) -> (Output, Vec<String>) {
    let log = store.with_extension("log");
    let _ = fs::remove_file(&log);
    let tinymold = env!("CARGO_BIN_EXE_tinymold");
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["test", "--lib", "--offline"])
        .current_dir(root)
        .env("RUSTC_WRAPPER", tinymold)
        .env(runner_variable(), format!("{tinymold} run-test"))
        .env("TINYMOLD_STORE", store)
        .env("TINYMOLD_LOG", &log)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("TINYMOLD_TEST_CACHE")
        .output()
        .unwrap();
    let lines = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("test-"))
        .map(str::to_owned)
        .collect();
    (output, lines)
}

/// `CARGO_TARGET_<TRIPLE>_RUNNER` for the host, the runner cargo uses for
/// tests built for it.
fn runner_variable() -> String {
    let version = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg("-vV")
        .output()
        .unwrap();
    let text = String::from_utf8(version.stdout).unwrap();
    let host = text
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    format!(
        "CARGO_TARGET_{}_RUNNER",
        host.to_uppercase().replace(['-', '.'], "_")
    )
}

/// Whether this test itself runs under `cargo llvm-cov`. Then
/// `LLVM_PROFILE_FILE` reaches the runner, which must treat every run as a
/// coverage run and never cache it; it is kept, so the instrumented
/// `tinymold` writes its own profile where coverage collects it rather than
/// into a package, where it would count as a changed input.
fn under_coverage() -> bool {
    std::env::var_os("LLVM_PROFILE_FILE").is_some()
}

/// The crates whose runs had `outcome` (`test-hit` or `test-miss`).
fn crates(lines: &[String], outcome: &str) -> BTreeSet<String> {
    lines
        .iter()
        .filter(|line| line.split(' ').next() == Some(outcome))
        .map(|line| line.split(' ').nth(1).unwrap().to_owned())
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn cargo_test_reruns_exactly_the_binaries_an_edit_reaches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    let store = dir.path().join("store");
    workspace(&root);

    let (first, lines) = cargo_test(&root, &store);
    assert!(first.status.success(), "{first:?}");
    if under_coverage() {
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines
                .iter()
                .all(|line| line.starts_with("test-pass - ") && line.ends_with("a coverage run")),
            "{lines:?}"
        );
        return;
    }
    assert_eq!(
        crates(&lines, "test-miss"),
        set(&["alpha", "beta", "gamma"])
    );

    // Nothing changed: every run is replayed, with its output.
    let (second, lines) = cargo_test(&root, &store);
    assert!(second.status.success(), "{second:?}");
    assert_eq!(crates(&lines, "test-hit"), set(&["alpha", "beta", "gamma"]));
    assert!(crates(&lines, "test-miss").is_empty(), "{lines:?}");
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert_eq!(
        stdout.matches("test result: ok. 1 passed").count(),
        3,
        "{stdout}"
    );

    // Edit alpha's logic: alpha and beta, which links it, run again; gamma
    // is replayed.
    // Written now, not back-dated like the fixture: cargo rebuilds only
    // what is newer than its outputs.
    fs::write(
        root.join("alpha/src/lib.rs"),
        alpha_source("\"1\".parse().unwrap()"),
    )
    .unwrap();
    let (third, lines) = cargo_test(&root, &store);
    assert!(third.status.success(), "{third:?}");
    assert_eq!(crates(&lines, "test-miss"), set(&["alpha", "beta"]));
    assert_eq!(crates(&lines, "test-hit"), set(&["gamma"]));
}

#[test]
fn a_failing_test_fails_again_instead_of_being_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    let store = dir.path().join("store");
    workspace(&root);
    write(&root.join("alpha/src/lib.rs"), &alpha_source("7"));
    for _ in 0..2 {
        let (output, lines) = cargo_test(&root, &store);
        assert!(!output.status.success());
        if under_coverage() {
            continue;
        }
        assert!(
            lines.iter().any(|line| line.starts_with("test-miss alpha ")
                && line.ends_with("not recorded: the run failed")),
            "{lines:?}"
        );
    }
}

#[test]
fn run_test_passes_arguments_and_exit_status_through() {
    let dir = tempfile::tempdir().unwrap();
    // A "binary" tinymold did not build runs unchanged, arguments and all.
    let script = dir.path().join("fake-test");
    write(&script, "#!/bin/sh\necho \"args: $*\"\nexit 3\n");
    if cfg!(unix) {
        Command::new("chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_tinymold"))
            .args(["run-test"])
            .arg(&script)
            .args(["--exact", "a b", "--quiet"])
            .env("TINYMOLD_STORE", dir.path().join("store"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "args: --exact a b --quiet\n"
        );
    }
}
