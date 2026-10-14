//! End-to-end tests of `RUSTC_WRAPPER=tinymold`: the real binary wrapping the
//! real rustc, in worktrees at different paths sharing one store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

fn rustc() -> PathBuf {
    std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from)
}

/// A worktree at `root` holding `source` as `src/lib.rs`, saved before the
/// build: its mtime is pushed back past any filesystem tick, since the
/// wrapper does not store a compile whose input may have changed while it
/// ran.
fn worktree(root: &Path, source: &str) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("target/debug/deps")).unwrap();
    let path = root.join("src/lib.rs");
    fs::write(&path, source).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(10))
        .unwrap();
}

/// Compiles the worktree at `root` through the wrapper, the way cargo would,
/// and returns the wrapper's log line for it.
fn build(root: &Path, store: &Path, env: &[(&str, &str)]) -> String {
    let log = store.with_extension("log");
    let _ = fs::remove_file(&log);
    let status = Command::new(env!("CARGO_BIN_EXE_tinymold"))
        .arg(rustc())
        .args([
            "--crate-name",
            "demo",
            "--edition=2021",
            "src/lib.rs",
            "--crate-type",
            "lib",
            "--emit=dep-info,metadata,link",
            "-C",
            "extra-filename=-1",
            "--out-dir",
        ])
        .arg(root.join("target/debug/deps"))
        .current_dir(root)
        .env("TINYMOLD_STORE", store)
        .env("TINYMOLD_LOG", &log)
        .envs(env.iter().copied())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(root.join("target/debug/deps/libdemo-1.rlib").is_file());
    fs::read_to_string(&log).unwrap()
}

fn outcome(line: &str) -> &str {
    line.split(' ').next().unwrap()
}

#[test]
fn a_second_worktree_hits_and_an_env_dep_change_misses() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let source = "pub const VALUE: &str = env!(\"TM_VALUE\");\n";
    worktree(&dir.path().join("a"), source);
    worktree(&dir.path().join("second-worktree"), source);

    let a = build(&dir.path().join("a"), &store, &[("TM_VALUE", "1")]);
    assert_eq!(outcome(&a), "miss", "{a}");
    let b = build(
        &dir.path().join("second-worktree"),
        &store,
        &[("TM_VALUE", "1")],
    );
    assert_eq!(outcome(&b), "hit", "{b}");
    let changed = build(
        &dir.path().join("second-worktree"),
        &store,
        &[("TM_VALUE", "2")],
    );
    assert_eq!(outcome(&changed), "miss", "{changed}");

    // The dep-info cargo reads names the second worktree's own files.
    let dep_info = fs::read_to_string(
        dir.path()
            .join("second-worktree/target/debug/deps/demo-1.d"),
    )
    .unwrap();
    assert!(dep_info.contains("second-worktree"), "{dep_info}");
    assert!(!dep_info.contains("/a/target"), "{dep_info}");
}

#[test]
fn a_crate_that_bakes_in_its_own_path_misses_in_another_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let source = "pub const HOME: &str = env!(\"CARGO_MANIFEST_DIR\");\n";
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    worktree(&a, source);
    worktree(&b, source);

    let first = build(&a, &store, &[("CARGO_MANIFEST_DIR", a.to_str().unwrap())]);
    assert_eq!(outcome(&first), "miss");
    let again = build(&a, &store, &[("CARGO_MANIFEST_DIR", a.to_str().unwrap())]);
    assert_eq!(outcome(&again), "hit");
    let other = build(&b, &store, &[("CARGO_MANIFEST_DIR", b.to_str().unwrap())]);
    assert_eq!(outcome(&other), "miss", "{other}");
}

#[test]
fn queries_and_other_programs_pass_through() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let output = Command::new(env!("CARGO_BIN_EXE_tinymold"))
        .arg(rustc())
        .arg("-vV")
        .env("TINYMOLD_STORE", dir.path().join("store"))
        .env("TINYMOLD_LOG", &log)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("rustc ")
    );
    assert!(fs::read_to_string(&log).unwrap().starts_with("pass - "));

    // Without any store location the compiler still runs, uncached.
    let output = Command::new(env!("CARGO_BIN_EXE_tinymold"))
        .arg(rustc())
        .arg("-vV")
        .env_remove("TINYMOLD_STORE")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("LOCALAPPDATA")
        .env_remove("HOME")
        .output()
        .unwrap();
    assert!(output.status.success());

    let missing = Command::new(env!("CARGO_BIN_EXE_tinymold"))
        .arg("/no/such/rustc")
        .arg("-vV")
        .env("TINYMOLD_STORE", dir.path().join("store"))
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
}

/// Runs `tinymold gc` with `args` on `store` and returns its output.
fn gc(store: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_tinymold"))
        .arg("--store")
        .arg(store)
        .arg("gc")
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn work_used_since_the_cutoff_still_hits_after_gc_and_expired_work_misses() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let root = dir.path().join("a");
    worktree(&root, "pub fn answer() -> u32 { 42 }\n");
    assert_eq!(outcome(&build(&root, &store, &[])), "miss");

    let kept = gc(&store, &["--max-age", "1h"]);
    assert!(kept.starts_with("gc: removed 0 actions"), "{kept}");
    assert_eq!(outcome(&build(&root, &store, &[])), "hit");

    // Everything is older than zero seconds once a second has passed.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let removed = gc(&store, &["--max-age", "0s"]);
    assert!(removed.starts_with("gc: removed 1 actions"), "{removed}");
    assert_eq!(outcome(&build(&root, &store, &[])), "miss");
}
