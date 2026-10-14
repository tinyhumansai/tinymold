//! Unit tests for the test runner: what is replayed, what runs again, and
//! what is never cached. Each test compiles a real test binary through the
//! wrapper, so the test-binary record is the wrapper's own.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::*;

fn rustc() -> PathBuf {
    std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from)
}

/// A package under `dir` whose tests run `body`, and a config with a log.
fn package(dir: &Path, body: &str) -> (PathBuf, Config) {
    let root = dir.join("pkg");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    write_source(&root, body);
    let mut config = Config::new(dir.join("store"));
    config.log = Some(dir.join("log"));
    (root, config)
}

/// Writes the crate root, dated in the past so the compile reading it may
/// be stored.
fn write_source(root: &Path, body: &str) {
    let source = root.join("src/lib.rs");
    fs::write(&source, format!("#[test]\nfn it_works() {{\n{body}\n}}\n")).unwrap();
    fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(10))
        .unwrap();
}

/// Compiles the package's test binary through the wrapper.
fn build(root: &Path, config: &Config) -> PathBuf {
    let out = root.join("target/debug/deps");
    fs::create_dir_all(&out).unwrap();
    let args: Vec<OsString> = [
        "--crate-name",
        "demo",
        "--edition=2021",
        "--test",
        "--emit=dep-info,link",
        "-C",
        "extra-filename=-1",
        "--out-dir",
    ]
    .iter()
    .map(OsString::from)
    .chain([out.clone().into(), root.join("src/lib.rs").into()])
    .collect();
    assert_eq!(crate::run(config, &rustc(), &args), 0);
    out.join(if cfg!(windows) { "demo-1.exe" } else { "demo-1" })
}

fn env(extra: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| name != "LLVM_PROFILE_FILE" && name != "TINYMOLD_TEST_CACHE")
        .collect();
    env.extend(
        extra
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
    );
    env
}

fn run(config: &Config, root: &Path, binary: &Path, args: &[&str], env: &[(OsString, OsString)]) -> i32 {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    run_test(
        config,
        &TestCommand {
            via: None,
            binary,
            args: &args,
            cwd: root,
            env,
        },
    )
}

/// The outcome and detail of every runner line in the log.
fn log_lines(config: &Config) -> Vec<String> {
    fs::read_to_string(config.log.as_ref().unwrap())
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("test-"))
        .map(|line| {
            let mut parts = line.splitn(4, ' ');
            let outcome = parts.next().unwrap();
            let _crate = parts.next();
            let _time = parts.next();
            format!("{outcome} {}", parts.next().unwrap_or("")).trim_end().to_owned()
        })
        .collect()
}

/// A test body that appends a line to the file `$MARKER`, so a test can
/// count how often the binary really ran.
const COUNTS_RUNS: &str = r#"use std::io::Write;
    let path = std::env::var("MARKER").unwrap();
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "ran").unwrap();"#;

fn runs(marker: &Path) -> usize {
    fs::read_to_string(marker).map_or(0, |text| text.lines().count())
}

#[test]
fn a_passing_run_is_replayed_without_running_the_binary() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    assert_eq!(run(&config, &root, &binary, &["--quiet"], &env), 0);
    assert_eq!(run(&config, &root, &binary, &["--quiet"], &env), 0);
    assert_eq!(runs(&marker), 1);
    let lines = log_lines(&config);
    assert!(lines[0].starts_with("test-miss recorded in"), "{lines:?}");
    assert_eq!(lines[1], "test-hit");

    // Other arguments are another run.
    assert_eq!(run(&config, &root, &binary, &["--exact", "it_works"], &env), 0);
    assert_eq!(runs(&marker), 2);
}

#[test]
fn a_failing_run_is_never_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), "assert_eq!(1, 2);");
    let binary = build(&root, &config);
    let env = env(&[]);
    assert_eq!(run(&config, &root, &binary, &["--quiet"], &env), 101);
    assert_eq!(run(&config, &root, &binary, &["--quiet"], &env), 101);
    assert_eq!(
        log_lines(&config),
        [
            "test-miss not recorded: the run failed",
            "test-miss not recorded: the run failed"
        ]
    );
}

#[test]
fn a_changed_binary_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    write_source(&root, &format!("{COUNTS_RUNS}\n    assert_eq!(2 + 2, 4);"));
    let rebuilt = build(&root, &config);
    assert_eq!(run(&config, &root, &rebuilt, &[], &env), 0);
    assert_eq!(runs(&marker), 2);
}

#[test]
fn a_changed_file_in_the_package_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    fs::create_dir_all(root.join("tests/data")).unwrap();
    fs::write(root.join("tests/data/case.json"), "{}").unwrap();
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 1);
    fs::write(root.join("tests/data/case.json"), "{\"changed\": true}").unwrap();
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 2);
}

#[test]
fn a_declared_input_outside_the_package_runs_again_when_changed() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\n\n[package.metadata.tinymold.test]\ninputs = [\"../fixtures\"]\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("fixtures")).unwrap();
    fs::write(dir.path().join("fixtures/a.txt"), "a").unwrap();
    let binary = build(&root, &config);
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 1);
    fs::write(dir.path().join("fixtures/a.txt"), "changed").unwrap();
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 2);
}

#[test]
fn a_changed_env_var_runs_again_but_an_ignored_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    let marker = dir.path().join("marker");
    let path = marker.to_str().unwrap();
    assert_eq!(run(&config, &root, &binary, &[], &env(&[("MARKER", path)])), 0);
    let ignored = env(&[("MARKER", path), ("COLUMNS", "71")]);
    assert_eq!(run(&config, &root, &binary, &[], &ignored), 0);
    assert_eq!(runs(&marker), 1);
    let changed = env(&[("MARKER", path), ("RUST_LOG", "debug")]);
    assert_eq!(run(&config, &root, &binary, &[], &changed), 0);
    assert_eq!(runs(&marker), 2);
}

#[test]
fn a_run_that_writes_into_its_package_is_not_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    let marker = root.join("ran.txt");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(
        log_lines(&config),
        ["test-miss not recorded: a file under the package changed during the run"]
    );
}

#[test]
fn a_missing_object_runs_the_test_again() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    let store = Store::open(&config.store).unwrap();
    for object in store.objects().unwrap() {
        store.remove(&object.digest).unwrap();
    }
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 2);
}

#[test]
fn bench_coverage_and_opted_out_runs_pass_through() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), "");
    let binary = build(&root, &config);
    assert_eq!(run(&config, &root, &binary, &["--bench"], &env(&[])), 0);
    let coverage = env(&[("LLVM_PROFILE_FILE", "/dev/null")]);
    assert_eq!(run(&config, &root, &binary, &[], &coverage), 0);
    let off = env(&[("TINYMOLD_TEST_CACHE", "0")]);
    assert_eq!(run(&config, &root, &binary, &[], &off), 0);
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\n[package.metadata.tinymold.test]\ncache = false\n",
    )
    .unwrap();
    assert_eq!(run(&config, &root, &binary, &[], &env(&[])), 0);
    assert_eq!(
        log_lines(&config),
        [
            "test-pass a bench run",
            "test-pass a coverage run",
            "test-pass TINYMOLD_TEST_CACHE=0",
            "test-pass the package sets cache = false",
        ]
    );
}

#[test]
fn a_binary_tinymold_did_not_build_passes_through() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), "");
    let out = dir.path().join("plain");
    fs::create_dir_all(&out).unwrap();
    let status = std::process::Command::new(rustc())
        .args(["--crate-name", "demo", "--edition=2021", "--test", "-o"])
        .arg(out.join("demo"))
        .arg(root.join("src/lib.rs"))
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(run(&config, &root, &out.join("demo"), &[], &env(&[])), 0);
    assert_eq!(
        log_lines(&config),
        ["test-pass not a test binary tinymold built"]
    );
}

#[test]
fn a_binary_that_cannot_be_hashed_still_runs_and_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), "");
    let missing = root.join("target/debug/deps/gone");
    assert_eq!(run(&config, &root, &missing, &[], &env(&[])), 1);
    assert!(log_lines(&config)[0].starts_with("test-error "));
}
