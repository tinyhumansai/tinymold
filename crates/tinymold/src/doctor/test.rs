//! Unit tests for the doctor report.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn path_with(names: &[&str]) -> (tempfile::TempDir, std::ffi::OsString) {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        std::fs::write(dir.path().join(executable(name)), b"").unwrap();
    }
    let path = std::env::join_paths([dir.path()]).unwrap();
    (dir, path)
}

#[test]
fn finds_only_tools_that_exist() {
    let (dir, path) = path_with(&["mold"]);

    assert_eq!(
        find_on_path("mold", &path),
        Some(dir.path().join(executable("mold")))
    );
    assert_eq!(find_on_path("wild", &path), None);
}

#[test]
fn reports_every_searched_tool_with_its_location() {
    let (dir, path) = path_with(&["wild", "sccache"]);
    let report = Report::gather(Some(&path), Path::new("rustc"), None);

    let found: Vec<_> = report
        .tools
        .iter()
        .filter(|tool| tool.path.is_some())
        .map(|tool| tool.name)
        .collect();
    assert_eq!(found, ["wild", "sccache"]);
    assert_eq!(report.tools.len(), TOOLS.len());
    assert!(
        report
            .to_string()
            .contains(&dir.path().display().to_string())
    );
}

#[test]
fn reads_the_toolchain_through_rustc() {
    let report = Report::gather(None, Path::new("rustc"), Some(PathBuf::from("/store")));

    assert!(report.rustc.as_deref().unwrap().starts_with("rustc "));
    assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
    let text = report.to_string();
    assert!(text.contains("store     /store"));
    assert!(text.contains("linkers and caches on PATH"));
}

#[test]
fn a_missing_rustc_is_reported_not_fatal() {
    let report = Report::gather(None, Path::new("/nonexistent/rustc"), None);

    assert_eq!(report.rustc, None);
    assert_eq!(report.rust_lld, None);
    let text = report.to_string();
    assert!(text.contains("rustc     not found"));
    assert!(text.contains("store     not found"));
}

#[test]
fn a_failing_rustc_yields_no_output() {
    assert_eq!(
        rustc_output(Path::new("rustc"), &["--definitely-not-a-flag"]),
        None
    );
}

#[test]
fn extracts_fields_from_verbose_version_output() {
    let text = "rustc 1.96.1\nhost: aarch64-apple-darwin\nrelease: 1.96.1\n";
    assert_eq!(field(text, "host: "), Some("aarch64-apple-darwin"));
    assert_eq!(field(text, "commit-hash: "), None);
}
