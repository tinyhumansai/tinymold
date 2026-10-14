//! Unit tests for object placement and its fallback chain.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn source_file(dir: &Path) -> std::path::PathBuf {
    let source = dir.join("object");
    fs::write(&source, b"artifact").unwrap();
    let mut permissions = fs::metadata(&source).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&source, permissions).unwrap();
    source
}

#[test]
fn copy_produces_an_independent_writable_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = source_file(dir.path());
    let destination = dir.path().join("copy");

    assert_eq!(
        place(&source, &destination, Strategy::Copy).unwrap(),
        Method::Copy
    );
    fs::write(&destination, b"rewritten").unwrap();
    assert_eq!(fs::read(&source).unwrap(), b"artifact");
}

#[test]
fn hardlink_shares_the_store_object_and_stays_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let source = source_file(dir.path());
    let destination = dir.path().join("link");

    assert_eq!(
        place(&source, &destination, Strategy::Hardlink).unwrap(),
        Method::Hardlink
    );
    assert!(fs::metadata(&destination).unwrap().permissions().readonly());
    assert_eq!(fs::read(&destination).unwrap(), b"artifact");
}

#[test]
fn auto_always_places_the_object() {
    let dir = tempfile::tempdir().unwrap();
    let source = source_file(dir.path());
    let destination = dir.path().join("auto");

    let method = place(&source, &destination, Strategy::Auto).unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"artifact");
    if method != Method::Hardlink {
        assert!(!fs::metadata(&destination).unwrap().permissions().readonly());
    }
}

#[test]
fn a_reflink_result_is_writable_when_the_filesystem_clones() {
    let dir = tempfile::tempdir().unwrap();
    let source = source_file(dir.path());
    let destination = dir.path().join("clone");

    // Whether this filesystem clones is a property of the test machine, so
    // only the outcome's consistency is asserted.
    match place(&source, &destination, Strategy::Reflink) {
        Ok(method) => {
            assert_eq!(method, Method::Reflink);
            fs::write(&destination, b"rewritten").unwrap();
            assert_eq!(fs::read(&source).unwrap(), b"artifact");
        }
        Err(error) => assert!(matches!(
            error,
            Error::Unsupported {
                method: Method::Reflink,
                ..
            }
        )),
    }
}

#[test]
fn falls_through_every_method_and_reports_the_last_failure() {
    let dir = tempfile::tempdir().unwrap();
    let source = source_file(dir.path());
    let destination = dir.path().join("missing-parent/out");

    let error = place(&source, &destination, Strategy::Auto).unwrap_err();
    assert!(matches!(
        error,
        Error::Unsupported { method: Method::Copy, path, .. } if path == destination
    ));
}

#[test]
fn an_empty_method_list_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let source = source_file(dir.path());

    let error = place_with(&source, &dir.path().join("out"), &[]).unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }));
}

#[test]
fn methods_render_lowercase() {
    let names: Vec<_> = Strategy::Auto
        .methods()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(names, ["reflink", "hardlink", "copy"]);
}

#[test]
fn a_private_copy_never_shares_the_source_inode() {
    let dir = tempfile::tempdir().unwrap();
    let source = source_file(dir.path());
    let destination = dir.path().join("private");

    let method = place_private_copy(&source, &destination).unwrap();
    assert_ne!(method, Method::Hardlink);
    fs::write(&destination, b"rewritten").unwrap();
    assert_eq!(fs::read(&source).unwrap(), b"artifact");
}

#[test]
fn restores_owner_write_access_to_a_read_only_file() {
    // Exercised directly because the reflink path that calls it only runs on
    // filesystems that clone (APFS, btrfs, XFS), not on ext4 or tmpfs.
    let dir = tempfile::tempdir().unwrap();
    let file = source_file(dir.path());
    assert!(fs::metadata(&file).unwrap().permissions().readonly());

    make_owner_writable(&file).unwrap();
    assert!(!fs::metadata(&file).unwrap().permissions().readonly());
}

#[test]
fn making_a_missing_file_writable_fails() {
    let dir = tempfile::tempdir().unwrap();
    assert!(make_owner_writable(&dir.path().join("missing")).is_err());
}

#[test]
fn clone_never_hardlinks_and_leaves_a_writable_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = source_file(dir.path());
    let destination = dir.path().join("clone");

    let method = place(&source, &destination, Strategy::Clone).unwrap();
    assert_ne!(method, Method::Hardlink);
    fs::write(&destination, b"rewritten").unwrap();
    assert_eq!(fs::read(&source).unwrap(), b"artifact");
}
