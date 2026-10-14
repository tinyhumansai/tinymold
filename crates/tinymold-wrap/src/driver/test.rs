//! Unit tests for recognizing clippy-driver and finding its configuration.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

use tinymold_key::Roots;

use super::*;

#[test]
fn clippy_driver_wrapping_rustc_is_recognized() {
    let driver = Path::new("/toolchain/bin/clippy-driver");
    assert!(is_clippy(
        driver,
        &["/toolchain/bin/rustc", "--crate-name", "a"]
    ));
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
    assert_eq!(
        find_config(&crate_dir),
        Some(dir.path().join("clippy.toml"))
    );

    fs::write(crate_dir.join("clippy.toml"), "").unwrap();
    fs::write(crate_dir.join(".clippy.toml"), "").unwrap();
    assert_eq!(
        find_config(&crate_dir),
        Some(crate_dir.join(".clippy.toml"))
    );
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

/// clippy-driver from the sysroot of the rustc cargo is testing with.
pub(crate) fn clippy_driver() -> PathBuf {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let sysroot = std::process::Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .unwrap();
    let sysroot = String::from_utf8(sysroot.stdout).unwrap();
    let name = format!("clippy-driver{}", std::env::consts::EXE_SUFFIX);
    PathBuf::from(sysroot.trim()).join("bin").join(name)
}

#[test]
fn the_identity_names_the_version_and_follows_the_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let workspace = dir.path().join("w");
    fs::create_dir_all(&workspace).unwrap();
    let roots = Roots::new(&workspace, Path::new("target/debug/deps"), None);
    let now = crate::clock::filesystem_now(dir.path()).unwrap();
    let identity = || clippy_identity(&store, &clippy_driver(), &workspace, &roots, now).unwrap();

    let without = identity();
    assert!(without.starts_with("clippy-driver\nclippy "), "{without}");
    assert!(without.ends_with("config none"), "{without}");

    let config = workspace.join("clippy.toml");
    fs::write(&config, "too-many-arguments-threshold = 3\n").unwrap();
    let with = identity();
    assert!(with.contains("config ${CWD}/clippy.toml "), "{with}");

    fs::write(&config, "too-many-arguments-threshold = 4\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&config)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(10))
        .unwrap();
    let edited = identity();
    assert_ne!(edited, with);
    assert_ne!(edited, without);
}
