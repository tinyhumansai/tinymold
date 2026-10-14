//! Unit tests for saving, listing, and restoring target snapshots.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, SystemTime};

use super::*;
use crate::tree::set_modified;

struct Fixture {
    dir: tempfile::TempDir,
    store: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        Self { dir, store }
    }

    /// A worktree at `name` with `main.rs` holding `source` and a built
    /// target whose output is newer than the source.
    fn worktree(&self, name: &str, source: &str) -> (PathBuf, PathBuf) {
        let workspace = self.dir.path().join(name);
        let target = workspace.join("target");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::write(workspace.join("src/main.rs"), source).unwrap();
        fs::write(target.join("debug/demo"), format!("built from {source}")).unwrap();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_modified(&workspace.join("src/main.rs"), old).unwrap();
        set_modified(&target.join("debug/demo"), old + Duration::from_secs(60)).unwrap();
        (workspace, target)
    }
}

#[test]
fn a_second_worktree_restores_a_snapshot_and_gets_the_built_mtimes() {
    let f = Fixture::new();
    let (first, first_target) = f.worktree("a", "fn main() {}");
    save(&f.store, "abc123", &first, &first_target).unwrap();

    // A fresh checkout of the same commit: same content, new mtime, no target.
    let second = f.dir.path().join("b");
    fs::create_dir_all(second.join("src")).unwrap();
    fs::write(second.join("src/main.rs"), "fn main() {}").unwrap();
    let target = second.join("target");

    let restored = restore(&f.store, None, &second, &target, false).unwrap();
    assert_eq!(restored.name, "abc123");
    assert_eq!((restored.matched, restored.files), (1, 1));
    let Restore::Applied(report) = restored.fresh else {
        panic!("expected the manifest to apply, got {:?}", restored.fresh);
    };
    assert_eq!(report.restored, 1);
    assert_eq!(
        fs::read_to_string(target.join("debug/demo")).unwrap(),
        "built from fn main() {}"
    );
    assert_eq!(
        fs::metadata(second.join("src/main.rs"))
            .unwrap()
            .modified()
            .unwrap(),
        fs::metadata(first.join("src/main.rs"))
            .unwrap()
            .modified()
            .unwrap()
    );
}

#[test]
fn the_best_match_is_the_snapshot_sharing_the_most_content() {
    let f = Fixture::new();
    let (a, a_target) = f.worktree("a", "version a");
    save(&f.store, "zz-a", &a, &a_target).unwrap();
    let (b, b_target) = f.worktree("b", "version b");
    save(&f.store, "aa-b", &b, &b_target).unwrap();

    let (c, _) = f.worktree("c", "version b");
    let restored = restore(&f.store, None, &c, &c.join("restored"), false).unwrap();
    assert_eq!(restored.name, "aa-b");
    assert_eq!(restored.matched, 1);
}

#[test]
fn saving_again_replaces_the_snapshot_and_list_shows_it() {
    let f = Fixture::new();
    assert_eq!(list(&f.store).unwrap(), Vec::<Snapshot>::new());
    let (a, a_target) = f.worktree("a", "one");
    save(&f.store, "main", &a, &a_target).unwrap();
    fs::write(a.join("src/lib.rs"), "two").unwrap();
    save(&f.store, "main", &a, &a_target).unwrap();
    // Leftovers and stray names in the snapshots directory are not snapshots.
    fs::create_dir_all(snapshots_dir(&f.store).join(".saving-x-1")).unwrap();

    assert_eq!(
        list(&f.store).unwrap(),
        [Snapshot {
            name: "main".into(),
            files: 2
        }]
    );
}

#[test]
fn restore_by_name_replaces_an_existing_target_only_when_asked() {
    let f = Fixture::new();
    let (a, a_target) = f.worktree("a", "one");
    save(&f.store, "main", &a, &a_target).unwrap();

    let error = restore(&f.store, Some("main"), &a, &a_target, false).unwrap_err();
    assert!(matches!(error, Error::AlreadyExists(_)));

    fs::write(a_target.join("debug/stale"), "from another branch").unwrap();
    let restored = restore(&f.store, Some("main"), &a, &a_target, true).unwrap();
    assert_eq!(restored.name, "main");
    assert!(!a_target.join("debug/stale").exists());
}

#[test]
fn restore_reports_missing_snapshots() {
    let f = Fixture::new();
    let (a, a_target) = f.worktree("a", "one");
    assert!(matches!(
        restore(&f.store, None, &a, &a.join("t"), false),
        Err(Error::NoSnapshots)
    ));
    assert!(matches!(
        restore(&f.store, Some("nope"), &a, &a_target, false),
        Err(Error::SnapshotNotFound(name)) if name == "nope"
    ));
}

#[test]
fn a_snapshot_without_a_manifest_scores_zero() {
    let f = Fixture::new();
    let bare = snapshots_dir(&f.store).join("bare");
    fs::create_dir_all(&bare).unwrap();
    let (a, _) = f.worktree("a", "one");
    assert_eq!(
        score(&bare, &a, &mut fresh::Digests::new()).unwrap(),
        (0, 0)
    );
    assert_eq!(list(&f.store).unwrap()[0].files, 0);
}

#[test]
fn snapshot_names_are_single_plain_path_components() {
    for good in ["main", "a1b2c3", "release-1.2_x"] {
        assert!(check_name(good).is_ok(), "{good}");
    }
    for bad in ["", ".hidden", "a/b", "..", "white space", "ü"] {
        assert!(
            matches!(check_name(bad), Err(Error::InvalidSnapshotName(n)) if n == bad),
            "{bad}"
        );
    }
    let f = Fixture::new();
    let (a, a_target) = f.worktree("a", "one");
    assert!(save(&f.store, "../escape", &a, &a_target).is_err());
    assert!(restore(&f.store, Some("../escape"), &a, &a_target, true).is_err());
}

#[test]
fn store_failures_carry_the_path() {
    let f = Fixture::new();
    fs::write(&f.store, b"not a directory").unwrap();
    let (a, a_target) = f.worktree("a", "one");
    assert!(matches!(
        save(&f.store, "main", &a, &a_target),
        Err(Error::Io {
            action: "create",
            ..
        })
    ));
    assert!(matches!(
        list(&f.store),
        Err(Error::Io { action: "read", .. })
    ));

    let file = f.dir.path().join("file");
    fs::write(&file, b"").unwrap();
    assert!(matches!(
        remove_tree(&file),
        Err(Error::Io {
            action: "remove",
            ..
        })
    ));
    remove_tree(&f.dir.path().join("missing")).unwrap();
}

#[test]
fn a_file_that_differs_from_the_snapshot_is_made_newer_than_its_outputs() {
    // A worktree checked out long ago, at other content, must not inherit a
    // newer snapshot's outputs as if they were built from its own files.
    let f = Fixture::new();
    let (a, a_target) = f.worktree("a", "fn main() { new() }");
    save(&f.store, "new", &a, &a_target).unwrap();
    let (old, _) = f.worktree("old", "fn main() { old() }");
    fs::remove_dir_all(old.join("target")).unwrap();
    let target = old.join("target");

    let restored = restore(&f.store, Some("new"), &old, &target, false).unwrap();
    let Restore::Applied(report) = restored.fresh else {
        panic!("expected the manifest to apply");
    };
    assert_eq!(report.changed, 1);
    let source = fs::metadata(old.join("src/main.rs"))
        .unwrap()
        .modified()
        .unwrap();
    let output = fs::metadata(target.join("debug/demo"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        source > output,
        "cargo would treat the stale output as current"
    );
}

#[test]
fn equal_mtimes_from_another_workspace_are_not_trusted() {
    let f = Fixture::new();
    let (a, a_target) = f.worktree("a", "version a");
    save(&f.store, "a", &a, &a_target).unwrap();
    // Same size and the very same mtime, different bytes.
    let (b, _) = f.worktree("b", "version b");
    let target = b.join("restored");

    let restored = restore(&f.store, Some("a"), &b, &target, false).unwrap();
    let Restore::Applied(report) = restored.fresh else {
        panic!("expected the manifest to apply");
    };
    assert_eq!((report.changed, report.unchanged), (1, 0));
}

#[test]
fn a_snapshot_whose_outputs_moved_is_refused_and_removed() {
    let f = Fixture::new();
    let (a, a_target) = f.worktree("a", "one");
    save(&f.store, "main", &a, &a_target).unwrap();
    let output = snapshots_dir(&f.store).join("main/debug/demo");
    set_modified(&output, SystemTime::now()).unwrap();

    let target = a.join("elsewhere");
    let error = restore(&f.store, Some("main"), &a, &target, false).unwrap_err();
    assert!(matches!(error, Error::SnapshotUnusable(name) if name == "main"));
    assert!(!target.exists());
}

#[test]
fn replacing_keeps_a_valid_target_as_an_automatic_snapshot() {
    let f = Fixture::new();
    let (a, a_target) = f.worktree("a", "one");
    save(&f.store, "main", &a, &a_target).unwrap();
    fs::write(a_target.join("debug/branch-b-output"), "b").unwrap();
    fresh::record(&a, &a_target).unwrap();

    let restored = restore(&f.store, Some("main"), &a, &a_target, true).unwrap();
    let kept = restored.kept.unwrap();
    assert!(kept.starts_with("auto-"), "{kept}");
    let kept_dir = snapshots_dir(&f.store).join(&kept);
    assert!(kept_dir.join("debug/branch-b-output").exists());
    assert!(!a_target.join("debug/branch-b-output").exists());

    // The same outputs set aside again are not stored twice.
    fs::write(a_target.join("debug/branch-b-output"), "b").unwrap();
    set_modified(
        &a_target.join("debug/branch-b-output"),
        fs::metadata(kept_dir.join("debug/branch-b-output"))
            .unwrap()
            .modified()
            .unwrap(),
    )
    .unwrap();
    fresh::record(&a, &a_target).unwrap();
    assert_eq!(set_aside(&f.store, &a_target).unwrap(), Some(kept));
    assert!(!a_target.exists());
}

#[test]
fn replacing_deletes_a_target_without_a_valid_manifest() {
    let f = Fixture::new();
    let (a, a_target) = f.worktree("a", "one");
    save(&f.store, "main", &a, &a_target).unwrap();
    // Built since the record: its manifest no longer describes it.
    fs::write(a_target.join("debug/newer"), "x").unwrap();

    let restored = restore(&f.store, Some("main"), &a, &a_target, true).unwrap();
    assert_eq!(restored.kept, None);
    assert!(!a_target.join("debug/newer").exists());
}
