//! Unit tests for directory-tree cloning.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, SystemTime};

use super::*;

/// A small `target/`-like tree with a nested file, an old mtime, and (on
/// Unix) a symlink.
fn sample_tree(root: &Path) -> SystemTime {
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    fs::create_dir_all(root.join("debug/deps")).unwrap();
    fs::write(root.join("debug/deps/libdemo.rlib"), b"rlib bytes").unwrap();
    fs::write(root.join("CACHEDIR.TAG"), b"tag").unwrap();
    set_modified(&root.join("debug/deps/libdemo.rlib"), old).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("deps/libdemo.rlib", root.join("debug/demo")).unwrap();
    old
}

fn assert_cloned(source: &Path, destination: &Path, old: SystemTime) {
    let rlib = destination.join("debug/deps/libdemo.rlib");
    assert_eq!(fs::read(&rlib).unwrap(), b"rlib bytes");
    assert_eq!(fs::metadata(&rlib).unwrap().modified().unwrap(), old);
    assert_eq!(fs::read(destination.join("CACHEDIR.TAG")).unwrap(), b"tag");
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(destination.join("debug/demo")).unwrap(),
        Path::new("deps/libdemo.rlib")
    );
    // The clone is independent: writing it leaves the source untouched.
    fs::write(&rlib, b"rewritten").unwrap();
    assert_eq!(
        fs::read(source.join("debug/deps/libdemo.rlib")).unwrap(),
        b"rlib bytes"
    );
}

#[test]
fn clones_a_tree_with_contents_mtimes_and_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("target");
    let old = sample_tree(&source);
    let destination = dir.path().join("clone");

    let method = clone_tree(&source, &destination).unwrap();
    assert_cloned(&source, &destination, old);
    if cfg!(not(target_vendor = "apple")) {
        assert!(matches!(method, TreeClone::Files { .. }));
    }
}

#[test]
fn the_file_walk_alone_preserves_contents_and_mtimes() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("target");
    let old = sample_tree(&source);
    let destination = dir.path().join("walked");

    let mut counts = (0, 0);
    walk(&source, &destination, true, &mut counts).unwrap();
    assert_eq!(counts.0 + counts.1, 2);
    assert_cloned(&source, &destination, old);

    let copied = dir.path().join("copied");
    let mut counts = (0, 0);
    walk(&source, &copied, false, &mut counts).unwrap();
    assert_eq!(counts, (0, 2));
    assert_cloned(&source, &copied, old);
}

#[test]
fn refuses_a_source_that_is_not_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, b"").unwrap();

    let error = clone_tree(&file, &dir.path().join("out")).unwrap_err();
    assert!(matches!(error, Error::NotADirectory(path) if path == file));
    let missing = clone_tree(&dir.path().join("missing"), &dir.path().join("out"));
    assert!(matches!(missing, Err(Error::NotADirectory(_))));
}

#[test]
fn refuses_an_existing_destination() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("target");
    fs::create_dir(&source).unwrap();
    let destination = dir.path().join("exists");
    fs::create_dir(&destination).unwrap();

    let error = clone_tree(&source, &destination).unwrap_err();
    assert!(matches!(error, Error::AlreadyExists(path) if path == destination));
}

#[test]
fn a_walk_into_a_missing_parent_reports_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("target");
    fs::create_dir(&source).unwrap();
    let destination = dir.path().join("no/such/parent");

    let error = walk(&source, &destination, true, &mut (0, 0)).unwrap_err();
    assert!(matches!(error, Error::Io { action: "create", path, .. } if path == destination));
}

#[test]
fn a_walk_of_a_missing_source_reports_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("gone");
    let error = walk(&source, &dir.path().join("out"), true, &mut (0, 0)).unwrap_err();
    assert!(matches!(error, Error::Io { action: "read", path, .. } if path == source));
}

#[test]
fn a_whole_tree_clone_that_cannot_happen_falls_back_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("target");
    fs::create_dir(&source).unwrap();
    let destination = dir.path().join("no/such/parent/clone");
    assert!(!clone_whole(&source, &destination));
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn set_modified_works_on_a_read_only_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ro");
    fs::write(&file, b"").unwrap();
    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&file, permissions).unwrap();

    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    set_modified(&file, when).unwrap();
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), when);
    assert!(set_modified(&dir.path().join("missing"), when).is_err());
}

#[test]
fn describes_how_the_tree_was_placed() {
    assert_eq!(TreeClone::Whole.to_string(), "whole-tree clone");
    assert_eq!(
        TreeClone::Files {
            reflinked: 3,
            copied: 1
        }
        .to_string(),
        "3 files reflinked, 1 copied"
    );
}
