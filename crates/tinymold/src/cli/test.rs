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

/// A git-free workspace with one source file and a built target.
fn built_workspace(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let workspace = root.join("ws");
    let target = workspace.join("target");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::create_dir_all(target.join("debug/.fingerprint/demo-1")).unwrap();
    fs::write(workspace.join("src/main.rs"), b"fn main() {}").unwrap();
    fs::write(target.join("debug/demo"), b"binary").unwrap();
    fs::write(target.join("debug/.fingerprint/demo-1/bin-demo"), b"0123").unwrap();
    (workspace, target)
}

#[test]
fn fresh_record_then_restore_reports_counts() {
    let dir = tempfile::tempdir().unwrap();
    let (workspace, target) = built_workspace(dir.path());
    let at = ["--workspace", s(&workspace), "--target-dir", s(&target)];

    let (result, out) = exec(&[&["fresh", "restore"][..], &at].concat());
    assert_eq!(result.unwrap(), Outcome::Absent);
    assert!(out.starts_with("fresh: no manifest in "), "{out}");

    let (result, out) = exec(&[&["fresh", "record"][..], &at].concat());
    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(
        out.starts_with("recorded 1 files (1 hashed, 0 skipped) in "),
        "{out}"
    );

    let (result, out) = exec(&[&["fresh", "restore"][..], &at].concat());
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(
        out,
        "fresh: 0 restored, 1 unchanged, 0 changed, 0 missing\n"
    );

    // A build since the record writes a new fingerprint.
    fs::write(target.join("debug/.fingerprint/demo-1/lib-demo"), b"").unwrap();
    let (result, out) = exec(&[&["fresh", "restore"][..], &at].concat());
    assert_eq!(result.unwrap(), Outcome::Absent);
    assert_eq!(
        out,
        "fresh: the target changed since the last record; nothing restored\n"
    );
}

#[test]
fn snapshot_save_list_and_restore_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let (workspace, target) = built_workspace(dir.path());
    let at = ["--workspace", s(&workspace), "--target-dir", s(&target)];

    let (result, out) =
        exec(&[&["--store", s(&store), "snapshot", "save", "main"][..], &at].concat());
    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(out.starts_with("saved snapshot main ("), "{out}");

    let (result, out) = exec(&["--store", s(&store), "snapshot", "list"]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(out, "main  1 files\n");

    let (result, _) = exec(&[&["--store", s(&store), "snapshot", "restore"][..], &at].concat());
    assert!(matches!(result, Err(Error::AlreadyExists(_))));

    let (result, out) = exec(
        &[
            &["--store", s(&store), "snapshot", "restore", "--replace"][..],
            &at,
        ]
        .concat(),
    );
    assert_eq!(result.unwrap(), Outcome::Success);
    let mut lines = out.lines();
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with("restored snapshot main (")
    );
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with("kept the previous target as snapshot auto-")
    );
    assert_eq!(
        lines.next(),
        Some("fresh: 0 restored, 1 unchanged, 0 changed, 0 missing")
    );
    assert!(out.contains("1 of 1 files match)"), "{out}");
}

#[test]
fn snapshot_save_without_a_name_needs_a_git_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let (workspace, target) = built_workspace(dir.path());
    let (result, _) = exec(&[
        "--store",
        s(&dir.path().join("store")),
        "snapshot",
        "save",
        "--workspace",
        s(&workspace),
        "--target-dir",
        s(&target),
    ]);
    assert!(matches!(result, Err(Error::NoSnapshotName)));
}

#[test]
fn git_head_names_the_checked_out_commit() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(git_head(dir.path()), None);
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
            .args(args)
            .output()
            .unwrap()
    };
    if !git(&["init", "-q"]).status.success() {
        return; // no git on this machine; the None case above still ran
    }
    git(&["commit", "-q", "--allow-empty", "-m", "c"]);
    let head = git_head(dir.path()).unwrap();
    assert_eq!(head.len(), 40, "{head}");
}

#[test]
fn locations_default_to_the_cwd_and_cargo_target_dir() {
    let none = |_: &str| None;
    let location = Location {
        workspace: None,
        target_dir: None,
    };
    assert_eq!(
        location.resolve(none),
        (PathBuf::from("."), PathBuf::from("./target"))
    );

    let env = |name: &str| (name == "CARGO_TARGET_DIR").then(|| "/shared/target".into());
    let location = Location {
        workspace: Some("/ws".into()),
        target_dir: None,
    };
    assert_eq!(location.resolve(env).1, PathBuf::from("/shared/target"));

    let relative = |name: &str| (name == "CARGO_TARGET_DIR").then(|| "out".into());
    assert_eq!(location.resolve(relative).1, PathBuf::from("/ws/out"));

    let location = Location {
        workspace: Some("/ws".into()),
        target_dir: Some("t".into()),
    };
    assert_eq!(location.resolve(env).1, PathBuf::from("/ws/t"));
}

#[test]
fn snapshot_commands_need_a_store() {
    let cli = Cli {
        store: None,
        command: Command::Snapshot(SnapshotCommand::List),
    };
    // Only meaningful when no default store can be derived; otherwise the
    // command runs against the real store, which this test must not touch.
    if store_dir(&cli).is_none() {
        let result = run(&cli, &mut Vec::new());
        assert!(matches!(result, Err(Error::NoStoreLocation)));
    }
}

#[test]
fn gc_reports_what_it_removed_from_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    Store::open(&store).unwrap();

    let (result, out) = exec(&[
        "--store",
        s(&store),
        "gc",
        "--max-age",
        "14d",
        "--max-size",
        "10G",
        "--snapshots-max-age",
        "30d",
        "--dry-run",
    ]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(out.starts_with("gc: would remove 0 actions"), "{out}");
    assert!(out.contains("gc: kept 0 actions, 0 objects, 0 B"), "{out}");
}

#[test]
fn gc_rejects_an_unparsable_age_or_size() {
    let parse = |args: &[&str]| {
        Cli::try_parse_from(std::iter::once("tinymold").chain(args.iter().copied()))
            .map(|_| ())
            .unwrap_err()
            .to_string()
    };
    assert!(parse(&["gc", "--max-age", "2w"]).contains("invalid duration"));
    assert!(parse(&["gc", "--max-size", "lots"]).contains("invalid size"));
}

#[test]
fn stats_summarizes_a_log_and_checks_the_hit_rate() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    fs::write(&log, "hit a 1.0ms \nmiss b 900.0ms stored in 1.0ms\n").unwrap();

    let (result, out) = exec(&["stats", "--log", s(&log)]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(out.starts_with("50.0% hit rate"), "{out}");

    let (result, out) = exec(&[
        "stats",
        "--log",
        s(&log),
        "--markdown",
        "--min-hit-rate",
        "50",
    ]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(out.starts_with("### tinymold"), "{out}");

    let (result, _) = exec(&["stats", "--log", s(&log), "--min-hit-rate", "90"]);
    assert_eq!(result.unwrap(), Outcome::Absent);
}

#[test]
fn stats_of_a_log_not_written_yet_has_no_hit_rate() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("none");
    let (result, out) = exec(&["stats", "--log", s(&log)]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(out.starts_with("no cacheable compiles"), "{out}");
    let (result, _) = exec(&["stats", "--log", s(&log), "--min-hit-rate", "1"]);
    assert_eq!(result.unwrap(), Outcome::Absent);
}

#[test]
fn stats_fails_on_a_log_it_cannot_read() {
    let dir = tempfile::tempdir().unwrap();
    let (result, _) = exec(&["stats", "--log", s(dir.path())]);
    assert!(matches!(result, Err(Error::Io { action: "read", .. })));
}

fn vars(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<std::ffi::OsString> {
    let pairs = pairs.to_vec();
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| std::ffi::OsString::from(value))
    }
}

#[test]
fn doctor_describes_the_remote_without_its_credential() {
    assert_eq!(remote_summary(vars(&[])), None);
    let anonymous = remote_summary(vars(&[("TINYMOLD_REMOTE_URL", "grpc://cache:9092")])).unwrap();
    let with_auth = remote_summary(vars(&[
        ("TINYMOLD_REMOTE_URL", "grpc://cache:9092"),
        ("TINYMOLD_REMOTE_AUTH", "ci:s3cret"),
    ]))
    .unwrap();
    let broken = remote_summary(vars(&[("TINYMOLD_REMOTE_URL", "cache")])).unwrap();
    if cfg!(feature = "remote") {
        assert_eq!(anonymous, "grpc://cache:9092 (anonymous)");
        assert_eq!(with_auth, "grpc://cache:9092 (with credential)");
        assert!(broken.starts_with("cache (invalid remote URL"), "{broken}");
    } else {
        assert!(anonymous.ends_with("(ignored: built without the remote feature)"));
    }
    assert!(!with_auth.contains("s3cret"));
}

#[test]
fn remote_commands_need_a_remote() {
    // The test environment configures no remote.
    if std::env::var_os("TINYMOLD_REMOTE_URL").is_none() {
        let (result, _) = exec(&["remote", "check"]);
        assert!(matches!(result, Err(Error::NoRemote)));
        let (result, _) = exec(&["remote", "push", "--since", "0"]);
        assert!(matches!(result, Err(Error::NoRemote)));
    }
}
