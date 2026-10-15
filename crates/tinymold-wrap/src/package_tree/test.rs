//! Unit tests for listing the files a test run may read.

#![allow(clippy::unwrap_used)]

use std::fs;

use super::*;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn names(files: &[(String, Digest)]) -> Vec<&str> {
    files.iter().map(|(name, _)| name.as_str()).collect()
}

#[test]
fn lists_every_file_but_git_and_target_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pkg");
    write(&root.join("Cargo.toml"), "[package]");
    write(&root.join("src/lib.rs"), "fn f() {}");
    write(&root.join("tests/data/a.json"), "{}");
    write(&root.join(".git/HEAD"), "ref");
    // A target dir holding the binary, and another one marked as a cache.
    write(&root.join("target/debug/deps/demo-1"), "bin");
    write(&root.join("target-ra/CACHEDIR.TAG"), "Signature");
    write(&root.join("target-ra/debug/x"), "x");
    let binary = root.join("target/debug/deps/demo-1");
    let store = dir.path().join("store");
    let files = package_files(&store, &root, &binary, u64::MAX).unwrap();
    assert_eq!(
        names(&files),
        ["Cargo.toml", "src/lib.rs", "tests/data/a.json"]
    );
    assert_eq!(files[2].1, Digest::of_bytes(b"{}"));
}

#[test]
fn a_changed_or_added_file_changes_the_listing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pkg");
    write(&root.join("tests/data/a.json"), "{}");
    let store = dir.path().join("store");
    let binary = dir.path().join("target/deps/t");
    let before = package_files(&store, &root, &binary, u64::MAX).unwrap();
    write(&root.join("tests/data/a.json"), "{\"a\":1}");
    let changed = package_files(&store, &root, &binary, u64::MAX).unwrap();
    assert_ne!(before, changed);
    write(&root.join("tests/data/b.json"), "{}");
    let added = package_files(&store, &root, &binary, u64::MAX).unwrap();
    assert_eq!(added.len(), 2);
}

#[cfg(unix)]
#[test]
fn a_symlink_is_named_by_its_target_and_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pkg");
    write(&dir.path().join("outside/big.bin"), "big");
    fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink("../outside", root.join("link")).unwrap();
    let store = dir.path().join("store");
    let files = package_files(&store, &root, &dir.path().join("t"), u64::MAX).unwrap();
    assert_eq!(names(&files), ["link"]);
    assert_eq!(files[0].1, Digest::of_bytes(b"symlink:../outside"));
}

#[test]
fn declared_inputs_cover_files_directories_and_missing_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws/pkg");
    write(&dir.path().join("ws/fixtures/one.txt"), "1");
    write(&dir.path().join("ws/fixtures/deep/two.txt"), "2");
    write(&dir.path().join("ws/schema.sql"), "create");
    fs::create_dir_all(&root).unwrap();
    let store = dir.path().join("store");
    let declared = [
        "../fixtures".to_owned(),
        "../schema.sql".to_owned(),
        "../gone".to_owned(),
    ];
    let files = declared_files(&store, &root, &declared, u64::MAX).unwrap();
    assert_eq!(
        names(&files),
        [
            "../fixtures/deep/two.txt",
            "../fixtures/one.txt",
            "../schema.sql",
            "../gone!missing"
        ]
    );
    // A missing input that appears later changes the listing.
    write(&dir.path().join("ws/gone"), "here");
    let later = declared_files(&store, &root, &declared, u64::MAX).unwrap();
    assert_ne!(files, later);
}

#[test]
fn a_target_dir_nested_in_a_test_dir_hides_only_itself() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pkg");
    write(&root.join("tests/fixtures/a.txt"), "a");
    write(&root.join("tests/target/debug/deps/demo-1"), "bin");
    write(&root.join("tests/target/debug/build/x/out/gen.rs"), "gen");
    let binary = root.join("tests/target/debug/deps/demo-1");
    let store = dir.path().join("store");
    let files = package_files(&store, &root, &binary, u64::MAX).unwrap();
    assert_eq!(names(&files), ["tests/fixtures/a.txt"]);
}

#[cfg(unix)]
#[test]
fn making_a_file_executable_changes_the_listing() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pkg");
    let script = root.join("tests/run.sh");
    write(&script, "#!/bin/sh\n");
    let store = dir.path().join("store");
    let binary = dir.path().join("target/debug/deps/t");
    let plain = package_files(&store, &root, &binary, u64::MAX).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let executable = package_files(&store, &root, &binary, u64::MAX).unwrap();
    assert_ne!(plain, executable);
    assert_eq!(names(&executable), ["tests/run.sh!x"]);
    let declared = declared_files(&store, &root, &["tests".to_owned()], u64::MAX).unwrap();
    assert_eq!(names(&declared), ["tests/run.sh!x"]);
}
