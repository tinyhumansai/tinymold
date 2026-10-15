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
    build_with(root, config, &[])
}

/// Compiles the package's test binary through the wrapper, with `extra`
/// arguments.
fn build_with(root: &Path, config: &Config, extra: &[OsString]) -> PathBuf {
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
    .chain(extra.iter().cloned())
    .collect();
    assert_eq!(crate::run(config, &rustc(), &args), 0);
    out.join(if cfg!(windows) {
        "demo-1.exe"
    } else {
        "demo-1"
    })
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

fn run(
    config: &Config,
    root: &Path,
    binary: &Path,
    args: &[&str],
    env: &[(OsString, OsString)],
) -> i32 {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    run_test(
        config,
        &TestCommand {
            via: None,
            binary,
            args: &args,
            cwd: root,
            env,
            stdin_has_input: false,
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
            format!("{outcome} {}", parts.next().unwrap_or(""))
                .trim_end()
                .to_owned()
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
    assert_eq!(
        run(&config, &root, &binary, &["--exact", "it_works"], &env),
        0
    );
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
    assert_eq!(
        run(&config, &root, &binary, &[], &env(&[("MARKER", path)])),
        0
    );
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

fn run_via(
    config: &Config,
    root: &Path,
    binary: &Path,
    via: &Path,
    env: &[(OsString, OsString)],
) -> i32 {
    run_test(
        config,
        &TestCommand {
            via: Some(via),
            binary,
            args: &[],
            cwd: root,
            env,
            stdin_has_input: false,
        },
    )
}

#[cfg(unix)]
fn script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_via_program_runs_the_binary_and_joins_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    let via = dir.path().join("via");
    script(&via, "#!/bin/sh\nexec \"$@\"\n");
    assert_eq!(run_via(&config, &root, &binary, &via, &env), 0);
    assert_eq!(run_via(&config, &root, &binary, &via, &env), 0);
    assert_eq!(runs(&marker), 1);
    // Another runner is another run.
    script(&via, "#!/bin/sh\n# a different emulator\nexec \"$@\"\n");
    assert_eq!(run_via(&config, &root, &binary, &via, &env), 0);
    assert_eq!(runs(&marker), 2);
}

#[test]
fn a_via_program_that_cannot_start_fails_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), "");
    let binary = build(&root, &config);
    let missing = dir.path().join("no-such-emulator");
    assert_eq!(run_via(&config, &root, &binary, &missing, &env(&[])), 1);
    assert_eq!(
        run_test_uncached(&TestCommand {
            via: Some(&missing),
            binary: &binary,
            args: &[],
            cwd: &root,
            env: &env(&[]),
            stdin_has_input: false,
        }),
        1
    );
}

#[test]
fn run_test_uncached_runs_the_binary_as_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    for _ in 0..2 {
        let code = run_test_uncached(&TestCommand {
            via: None,
            binary: &binary,
            args: &[],
            cwd: &root,
            env: &env,
            stdin_has_input: false,
        });
        assert_eq!(code, 0);
    }
    assert_eq!(runs(&marker), 2);
}

#[test]
fn a_corrupt_run_record_runs_the_test_again() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    for entry in fs::read_dir(config.store.join("tests/run")).unwrap() {
        fs::write(entry.unwrap().path(), "garbage").unwrap();
    }
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 2);
}

#[test]
fn a_changed_dynamic_library_beside_the_binary_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    // As the wrapper records a binary linked against a dylib crate.
    let digest = Digest::of_file(&binary).unwrap();
    let record = TestBinary {
        crate_name: "demo".into(),
        dylibs: vec!["libplugin-1.dylib".into()],
        exes: Vec::new(),
        out_dirs: Vec::new(),
    };
    fs::write(TestBinary::path(&config.store, &digest), record.encode()).unwrap();
    let dylib = binary.with_file_name("libplugin-1.dylib");
    fs::write(&dylib, "v1").unwrap();
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 1);
    fs::write(&dylib, "version two").unwrap();
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 2);
}

#[test]
fn a_package_without_a_manifest_passes_through() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), "");
    let binary = build(&root, &config);
    fs::remove_file(root.join("Cargo.toml")).unwrap();
    assert_eq!(run(&config, &root, &binary, &[], &env(&[])), 0);
    assert_eq!(
        log_lines(&config),
        ["test-pass no Cargo.toml in the working directory"]
    );
}

#[cfg(unix)]
#[test]
fn arguments_and_env_vars_that_are_not_utf8_pass_through() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), "");
    let binary = build(&root, &config);
    let odd = OsString::from_vec(vec![b'-', b'-', 0xff]);
    let args = [OsString::from("--quiet"), odd.clone()];
    let code = run_test(
        &config,
        &TestCommand {
            via: None,
            binary: &binary,
            args: &args[..1],
            cwd: &root,
            env: &[(OsString::from("ODD"), odd)],
            stdin_has_input: false,
        },
    );
    assert_eq!(code, 0);
    let filtered: Vec<OsString> = args.to_vec();
    let _ = run_test(
        &config,
        &TestCommand {
            via: None,
            binary: &binary,
            args: &filtered,
            cwd: &root,
            env: &env(&[]),
            stdin_has_input: false,
        },
    );
    assert_eq!(
        log_lines(&config),
        [
            "test-pass an environment variable is not UTF-8",
            "test-pass an argument is not UTF-8"
        ]
    );
}

#[test]
fn a_changed_build_script_output_or_package_binary_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    // As the wrapper records a test compiled with OUT_DIR and
    // CARGO_BIN_EXE_tool set.
    let profile = root.join("target/debug");
    fs::create_dir_all(profile.join("build/demo-1/out")).unwrap();
    fs::write(profile.join("build/demo-1/out/table.bin"), "v1").unwrap();
    fs::write(profile.join("tool"), "tool v1").unwrap();
    let digest = Digest::of_file(&binary).unwrap();
    let record = TestBinary {
        crate_name: "demo".into(),
        dylibs: Vec::new(),
        exes: vec!["${PROFILE}/tool".into()],
        out_dirs: vec!["${PROFILE}/build/demo-1/out".into()],
    };
    fs::write(TestBinary::path(&config.store, &digest), record.encode()).unwrap();
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 1);
    fs::write(profile.join("build/demo-1/out/table.bin"), "version two").unwrap();
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 2);
    fs::write(profile.join("tool"), "tool version two").unwrap();
    assert_eq!(run(&config, &root, &binary, &[], &env), 0);
    assert_eq!(runs(&marker), 3);
}

#[test]
fn a_run_whose_stdin_may_carry_input_is_never_cached() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), COUNTS_RUNS);
    let binary = build(&root, &config);
    let marker = dir.path().join("marker");
    let env = env(&[("MARKER", marker.to_str().unwrap())]);
    let command = TestCommand {
        via: None,
        binary: &binary,
        args: &[],
        cwd: &root,
        env: &env,
        stdin_has_input: true,
    };
    assert_eq!(run_test(&config, &command), 0);
    assert_eq!(run_test(&config, &command), 0);
    assert_eq!(runs(&marker), 2);
    assert_eq!(
        log_lines(&config),
        [
            "test-pass stdin may carry input",
            "test-pass stdin may carry input"
        ]
    );
}

#[test]
fn a_cached_run_reads_an_empty_stdin() {
    // Whatever this process's stdin holds, the run the key describes reads
    // nothing from it.
    let dir = tempfile::tempdir().unwrap();
    let body = "let mut text = String::new();\n    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).unwrap();\n    assert_eq!(text, \"\");";
    let (root, config) = package(dir.path(), body);
    let binary = build(&root, &config);
    assert_eq!(run(&config, &root, &binary, &[], &env(&[])), 0);
    assert!(log_lines(&config)[0].starts_with("test-miss recorded in"));
}

#[test]
fn a_shared_library_in_a_native_search_path_of_the_build_turns_caching_off() {
    // A build script's shared library is loaded at run time, and the test
    // binary's bytes do not change when it does.
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), "");
    let native = root.join("target/debug/build/native-1/out");
    fs::create_dir_all(&native).unwrap();
    let search = |dir: &Path| [OsString::from("-L"), format!("native={}", dir.display()).into()];

    // A static library is part of the binary.
    fs::write(native.join("libnative.a"), "archive").unwrap();
    let binary = build_with(&root, &config, &search(&native));
    assert_eq!(run(&config, &root, &binary, &[], &env(&[])), 0);
    assert!(log_lines(&config)[0].starts_with("test-miss recorded in"));

    fs::write(native.join("libnative.so"), "shared").unwrap();
    write_source(&root, "assert_eq!(1, 1);");
    let binary = build_with(&root, &config, &search(&native));
    assert_eq!(run(&config, &root, &binary, &[], &env(&[])), 0);
    assert_eq!(
        log_lines(&config)[1],
        "test-pass not a test binary tinymold built"
    );
}

#[test]
fn a_miss_marks_the_test_binary_record_as_used() {
    let dir = tempfile::tempdir().unwrap();
    let (root, config) = package(dir.path(), "");
    let binary = build(&root, &config);
    let record = fs::read_dir(config.store.join("tests/bin"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let old = SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
    fs::File::options()
        .write(true)
        .open(&record)
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert_eq!(run(&config, &root, &binary, &[], &env(&[])), 0);
    let modified = fs::metadata(&record).unwrap().modified().unwrap();
    assert!(modified > old + Duration::from_secs(3600));
}
