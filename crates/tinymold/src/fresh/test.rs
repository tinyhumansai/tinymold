//! Unit tests for recording and restoring content freshness.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

/// A workspace with sources, a `.git` dir, a nested checkout, and a cargo
/// target directory holding one output.
struct Fixture {
    _dir: tempfile::TempDir,
    workspace: PathBuf,
    target: PathBuf,
}

const OLD: u64 = 1_700_000_000_000_000_000;
const BUILT: u64 = 1_700_000_100_000_000_000;

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("ws");
    let target = workspace.join("target");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::create_dir_all(workspace.join(".git")).unwrap();
    fs::create_dir_all(workspace.join("nested/.git")).unwrap();
    fs::create_dir_all(target.join("debug")).unwrap();
    fs::write(workspace.join("Cargo.toml"), b"[package]").unwrap();
    fs::write(workspace.join("src/main.rs"), b"fn main() {}").unwrap();
    fs::write(workspace.join(".git/HEAD"), b"ref").unwrap();
    fs::write(workspace.join("nested/lib.rs"), b"nested").unwrap();
    fs::write(target.join("CACHEDIR.TAG"), b"tag").unwrap();
    fs::write(target.join("debug/demo"), b"binary").unwrap();
    for file in ["Cargo.toml", "src/main.rs"] {
        set_modified(&workspace.join(file), from_nanos(OLD)).unwrap();
    }
    set_modified(&target.join("debug/demo"), from_nanos(BUILT)).unwrap();
    Fixture {
        _dir: dir,
        workspace,
        target,
    }
}

fn mtime(path: &Path) -> u64 {
    nanos(fs::metadata(path).unwrap().modified().unwrap())
}

fn touch(path: &Path) {
    set_modified(path, SystemTime::now()).unwrap();
}

#[test]
fn the_manifest_encoding_is_pinned() {
    let manifest = Manifest {
        recorded_ns: 2,
        stamp_ns: 1,
        entries: vec![Entry {
            path: "src/main.rs".into(),
            size: 0,
            mtime_ns: 3,
            digest: Digest::of_bytes(b""),
        }],
    };
    // The BLAKE3 digest of empty input is a published test vector.
    let expected = "tinymold-fresh 1\nrecorded 2\nstamp 1\n\
        3 0 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 src/main.rs\n";
    assert_eq!(manifest.encode(), expected);
    assert_eq!(
        Manifest::decode(expected, Path::new("m")).unwrap(),
        manifest
    );
}

#[test]
fn records_workspace_files_but_not_git_targets_or_nested_checkouts() {
    let f = fixture();
    let report = record(&f.workspace, &f.target).unwrap();
    assert_eq!(report.files, 2);
    assert_eq!(report.hashed, 2);
    assert_eq!(report.skipped, 0);

    let manifest = load(&f.target).unwrap().unwrap();
    let paths: Vec<_> = manifest.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["Cargo.toml", "src/main.rs"]);
    assert_eq!(manifest.stamp_ns, BUILT);
    assert_eq!(manifest.entries[1].mtime_ns, OLD);
}

#[test]
fn a_second_record_reuses_digests_of_unchanged_files() {
    let f = fixture();
    record(&f.workspace, &f.target).unwrap();
    fs::write(f.workspace.join("src/main.rs"), b"fn main() { edited }").unwrap();

    let report = record(&f.workspace, &f.target).unwrap();
    assert_eq!(report.files, 2);
    assert_eq!(report.hashed, 1);
}

#[test]
fn restore_undoes_a_touch_that_did_not_change_content() {
    let f = fixture();
    record(&f.workspace, &f.target).unwrap();
    let main = f.workspace.join("src/main.rs");
    touch(&main);
    assert_ne!(mtime(&main), OLD);

    let Restore::Applied(report) = restore(&f.workspace, &f.target).unwrap() else {
        panic!("expected the manifest to apply");
    };
    assert_eq!(report.restored, 1);
    assert_eq!(report.unchanged, 1);
    assert_eq!(mtime(&main), OLD);
}

#[test]
fn restore_leaves_changed_and_missing_files_alone() {
    let f = fixture();
    record(&f.workspace, &f.target).unwrap();
    let main = f.workspace.join("src/main.rs");
    // Same size, different bytes: only the digest can tell.
    fs::write(&main, b"fn main() {;").unwrap();
    fs::remove_file(f.workspace.join("Cargo.toml")).unwrap();
    let edited = mtime(&main);

    let Restore::Applied(report) = restore(&f.workspace, &f.target).unwrap() else {
        panic!("expected the manifest to apply");
    };
    assert_eq!((report.changed, report.missing, report.restored), (1, 1, 0));
    assert_eq!(mtime(&main), edited);

    fs::write(&main, b"a different size").unwrap();
    let Restore::Applied(report) = restore(&f.workspace, &f.target).unwrap() else {
        panic!("expected the manifest to apply");
    };
    assert_eq!(report.changed, 1);
}

#[test]
fn restore_refuses_after_a_build_changed_the_target() {
    // Edit, build, undo: the undone file must not get its old mtime back, or
    // cargo would think the edited build's outputs are still current.
    let f = fixture();
    record(&f.workspace, &f.target).unwrap();
    touch(&f.target.join("debug/demo"));
    touch(&f.workspace.join("src/main.rs"));

    assert_eq!(
        restore(&f.workspace, &f.target).unwrap(),
        Restore::TargetChanged
    );
    assert_ne!(mtime(&f.workspace.join("src/main.rs")), OLD);
}

#[test]
fn cargo_bookkeeping_files_do_not_move_the_stamp() {
    let f = fixture();
    record(&f.workspace, &f.target).unwrap();
    fs::write(f.target.join("debug/.cargo-lock"), b"").unwrap();
    fs::write(f.target.join(".rustc_info.json"), b"{}").unwrap();
    fs::write(f.target.join("debug/demo.d"), b"demo: src/main.rs").unwrap();
    assert!(matches!(
        restore(&f.workspace, &f.target).unwrap(),
        Restore::Applied(_)
    ));
}

#[test]
fn restore_without_a_manifest_does_nothing() {
    let f = fixture();
    assert_eq!(
        restore(&f.workspace, &f.target).unwrap(),
        Restore::NoManifest
    );
}

#[test]
fn a_missing_target_has_a_zero_stamp() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(stamp(&dir.path().join("missing")).unwrap(), 0);
}

#[test]
fn reading_a_target_that_is_a_file_fails_with_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("target");
    fs::write(&file, b"").unwrap();
    assert!(matches!(
        stamp(&file),
        Err(Error::Io { action: "read", .. })
    ));
    assert!(matches!(load(&file), Err(Error::Io { action: "read", .. })));
}

#[test]
fn a_corrupt_manifest_names_the_bad_line() {
    let path = Path::new("fresh");
    let cases = [
        ("", 1),
        ("tinymold-fresh 1\n", 2),
        ("tinymold-fresh 1\nrecorded x\n", 2),
        ("tinymold-fresh 1\nrecorded 1\nstamp\n", 3),
        ("tinymold-fresh 1\nrecorded 1\nstamp 1\n1 2 nothex a\n", 4),
        ("tinymold-fresh 1\nrecorded 1\nstamp 1\n1 2\n", 4),
    ];
    for (text, line) in cases {
        let error = Manifest::decode(text, path).unwrap_err();
        assert!(
            matches!(&error, Error::CorruptManifest { line: l, .. } if *l == line),
            "{text:?} -> {error}"
        );
    }
    let digest = Digest::of_bytes(b"");
    let no_path = format!("tinymold-fresh 1\nrecorded 1\nstamp 1\n1 2 {digest} \n");
    assert!(Manifest::decode(&no_path, path).is_err());
}

#[test]
fn a_corrupt_manifest_on_disk_is_reported() {
    let f = fixture();
    write_atomically(&manifest_path(&f.target), b"garbage").unwrap();
    assert!(matches!(
        restore(&f.workspace, &f.target),
        Err(Error::CorruptManifest { line: 1, .. })
    ));
}

#[test]
fn record_fails_on_an_unreadable_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let error = record(&dir.path().join("missing"), &dir.path().join("t")).unwrap_err();
    assert!(matches!(error, Error::Io { action: "read", .. }));
}

#[test]
fn write_atomically_reports_an_unwritable_location() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    fs::write(&blocker, b"").unwrap();
    let error = write_atomically(&blocker.join("sub/manifest"), b"x").unwrap_err();
    assert!(matches!(
        error,
        Error::Io {
            action: "create",
            ..
        }
    ));
}

#[test]
fn paths_that_cannot_be_one_manifest_line_are_skipped() {
    let root = Path::new("/ws");
    assert_eq!(
        relative(root, Path::new("/ws/a/b.rs")).as_deref(),
        Some("a/b.rs")
    );
    assert_eq!(relative(root, Path::new("/ws/bad\nname")), None);
    assert_eq!(relative(root, Path::new("/elsewhere")), None);
}

#[cfg(unix)]
#[test]
fn record_counts_files_with_unencodable_names_as_skipped() {
    let f = fixture();
    fs::write(f.workspace.join("line\nbreak.rs"), b"").unwrap();
    let report = record(&f.workspace, &f.target).unwrap();
    assert_eq!((report.files, report.skipped), (2, 1));
}

#[test]
fn times_convert_to_nanoseconds_and_back() {
    assert_eq!(nanos(from_nanos(OLD)), OLD);
    assert_eq!(nanos(UNIX_EPOCH - Duration::from_secs(1)), 0);
    assert_eq!(
        manifest_path(Path::new("t")),
        Path::new("t/.tinymold/fresh")
    );
}

#[test]
fn verified_restore_hashes_even_when_mtimes_match_and_memoizes() {
    let f = fixture();
    record(&f.workspace, &f.target).unwrap();
    let mut digests = Digests::new();
    let Restore::Applied(report) = apply(&f.workspace, &f.target, Some(&mut digests)).unwrap()
    else {
        panic!("expected the manifest to apply");
    };
    assert_eq!(report.unchanged, 2);
    assert_eq!(digests.len(), 2);

    // A memoized digest is used as-is: a wrong one reads as changed.
    digests.insert("src/main.rs".into(), None);
    let Restore::Applied(report) = apply(&f.workspace, &f.target, Some(&mut digests)).unwrap()
    else {
        panic!("expected the manifest to apply");
    };
    assert_eq!(report.changed, 1);
}

#[test]
fn a_changed_file_older_than_the_outputs_is_made_newer() {
    let f = fixture();
    record(&f.workspace, &f.target).unwrap();
    let main = f.workspace.join("src/main.rs");
    fs::write(&main, b"fn main() { 1 }").unwrap();
    set_modified(&main, from_nanos(OLD)).unwrap();

    let Restore::Applied(report) = restore(&f.workspace, &f.target).unwrap() else {
        panic!("expected the manifest to apply");
    };
    assert_eq!(report.changed, 1);
    assert!(mtime(&main) > BUILT);
}
