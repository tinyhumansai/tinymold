//! Unit tests for recognizing clippy-driver and finding its configuration.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use super::*;

#[test]
fn clippy_driver_wrapping_rustc_is_recognized() {
    let driver = Path::new("/toolchain/bin/clippy-driver");
    assert!(is_clippy(driver, &["/toolchain/bin/rustc", "--crate-name", "a"]));
    assert!(is_clippy(Path::new("clippy-driver.exe"), &["rustc.exe"]));
}

#[test]
fn other_programs_and_clippy_without_rustc_are_not() {
    assert!(!is_clippy(Path::new("rustc"), &["rustc"]));
    assert!(!is_clippy(Path::new("clippy-driver"), &["--version"]));
    assert!(!is_clippy::<&str>(Path::new("clippy-driver"), &[]));
}

#[test]
fn the_nearest_configuration_wins_and_the_dotted_name_first() {
    let dir = tempfile::tempdir().unwrap();
    let crate_dir = dir.path().join("crates/a");
    fs::create_dir_all(&crate_dir).unwrap();
    assert_eq!(find_config(&crate_dir), None);

    fs::write(dir.path().join("clippy.toml"), "").unwrap();
    assert_eq!(find_config(&crate_dir), Some(dir.path().join("clippy.toml")));

    fs::write(crate_dir.join("clippy.toml"), "").unwrap();
    fs::write(crate_dir.join(".clippy.toml"), "").unwrap();
    assert_eq!(find_config(&crate_dir), Some(crate_dir.join(".clippy.toml")));
}

#[test]
fn a_directory_named_like_a_configuration_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("a/clippy.toml")).unwrap();
    fs::write(dir.path().join(".clippy.toml"), "").unwrap();
    assert_eq!(
        find_config(&dir.path().join("a")),
        Some(dir.path().join(".clippy.toml"))
    );
}
