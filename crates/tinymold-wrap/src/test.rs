//! Unit tests for the wrapper entry point: what is passed through, served,
//! or fallen back from, and what the log says.

#![allow(clippy::unwrap_used)]

use std::fs;

use super::*;

fn rustc() -> PathBuf {
    std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from)
}

fn config(dir: &Path) -> Config {
    let mut config = Config::new(dir.join("store"));
    config.log = Some(dir.join("log"));
    config
}

fn log_lines(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn compile_args(dir: &Path, extra: &[&str]) -> Vec<OsString> {
    fs::create_dir_all(dir.join("out")).unwrap();
    fs::write(dir.join("lib.rs"), "pub fn f() {}\n").unwrap();
    let mut args: Vec<OsString> = ["--crate-name", "demo", "--crate-type", "lib"]
        .iter()
        .map(OsString::from)
        .collect();
    args.extend(["--emit=dep-info,metadata,link".into(), "--out-dir".into()]);
    args.push(dir.join("out").into());
    args.push(dir.join("lib.rs").into());
    args.extend(extra.iter().map(OsString::from));
    args
}

#[test]
fn a_compile_misses_then_hits_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let args = compile_args(dir.path(), &[]);
    assert_eq!(run(&config, &rustc(), &args), 0);
    assert_eq!(run(&config, &rustc(), &args), 0);
    let lines = log_lines(dir.path());
    assert!(
        lines[0].starts_with("miss demo ") && lines[0].contains("stored in"),
        "{lines:?}"
    );
    assert!(lines[1].starts_with("hit demo "), "{lines:?}");
}

#[test]
fn a_failed_compile_reports_rustc_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let args = compile_args(dir.path(), &[]);
    fs::write(dir.path().join("lib.rs"), "pub fn f( {\n").unwrap();
    assert_ne!(run(&config, &rustc(), &args), 0);
    assert!(log_lines(dir.path())[0].contains("not stored: rustc failed"));
}

#[test]
fn a_cache_error_falls_back_to_running_rustc() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let missing = format!("dep={}", dir.path().join("libdep.rlib").display());
    let args = compile_args(dir.path(), &["--extern", &missing]);
    // The cache cannot hash the missing artifact, so it steps aside and runs
    // rustc, which ignores an extern the crate never uses; cargo sees rustc's
    // success, and the cache's failure is only logged.
    assert_eq!(run(&config, &rustc(), &args), 0);
    assert!(log_lines(dir.path())[0].starts_with("error demo "));
}

#[test]
fn queries_other_programs_and_odd_arguments_pass_through() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    assert_eq!(run(&config, &rustc(), &["-vV".into()]), 0);
    assert!(log_lines(dir.path())[0].contains("asks rustc about itself"));

    assert_eq!(run(&config, Path::new("/no/such/program"), &[]), 1);
    assert!(log_lines(dir.path())[1].contains("not rustc"));
    assert_eq!(run_uncached(Path::new("/no/such/program"), &[]), 1);

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let odd = OsString::from_vec(vec![0xff]);
        // A program that cannot start, so only the classification runs.
        assert_eq!(run(&config, Path::new("/no/such/rustc"), &[odd]), 1);
        assert!(log_lines(dir.path())[2].contains("not UTF-8"));
    }

    // Without a log, nothing is written anywhere.
    let quiet = Config::new(dir.path().join("store"));
    assert_eq!(run(&quiet, &rustc(), &["-vV".into()]), 0);
}

#[test]
fn only_programs_named_rustc_are_cached() {
    assert!(is_rustc(Path::new("rustc")));
    assert!(is_rustc(Path::new("/toolchain/bin/rustc")));
    assert!(is_rustc(Path::new("rustc.exe")));
    assert!(!is_rustc(Path::new("clippy-driver")));
}

fn trace_steps(dir: &Path) -> Vec<(String, String)> {
    fs::read_to_string(dir.join("trace"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let field = |key: &str| {
                let start = line.find(&format!(r#""{key}":""#)).unwrap() + key.len() + 4;
                line[start..start + line[start..].find('"').unwrap()].to_owned()
            };
            (field("name"), field("outcome"))
        })
        .collect()
}

#[test]
fn a_trace_records_each_step_of_a_miss_and_a_hit() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.trace = Some(dir.path().join("trace"));
    let args = compile_args(dir.path(), &[]);
    assert_eq!(run(&config, &rustc(), &args), 0);
    assert_eq!(run(&config, &rustc(), &args), 0);
    let steps = trace_steps(dir.path());
    let miss: Vec<_> = steps
        .iter()
        .filter(|(_, outcome)| outcome == "miss")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        miss,
        [
            "invocation",
            "classify",
            "pre_key",
            "lookup",
            "rustc",
            "ingest"
        ]
    );
    let hit: Vec<_> = steps
        .iter()
        .filter(|(_, outcome)| outcome == "hit")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        hit,
        ["invocation", "classify", "pre_key", "lookup", "restore"]
    );
}

#[test]
fn a_trace_records_a_pass_through() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.trace = Some(dir.path().join("trace"));
    assert_eq!(run(&config, &rustc(), &["-V".into()]), 0);
    let steps = trace_steps(dir.path());
    let names: Vec<_> = steps.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["invocation", "classify", "pass_through"]);
    assert!(steps.iter().all(|(_, outcome)| outcome == "pass"));
}
