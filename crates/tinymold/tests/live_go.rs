//! Live test: Go's build cache through `tinymold gocacheprog`. Runs only
//! with `TINYMOLD_LIVE=1` and Go 1.24 or later on `PATH`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

fn live() -> bool {
    std::env::var_os("TINYMOLD_LIVE").is_some_and(|value| value == "1")
        && Command::new("go").arg("version").output().is_ok()
}

/// Builds the fixture with an empty `GOCACHE` of its own and tinymold as
/// `GOCACHEPROG`, and returns the packages go had to compile (`-v`).
fn build(dir: &Path, store: &Path, run: &str) -> String {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go");
    let program = format!(
        "{} gocacheprog --dir {}",
        env!("CARGO_BIN_EXE_tinymold"),
        store.display()
    );
    let output = Command::new("go")
        .args(["build", "-v", "-o"])
        .arg(dir.join(format!("fixture-{run}")))
        .arg(".")
        .current_dir(fixture)
        .env("GOCACHE", dir.join(format!("gocache-{run}")))
        .env("GOCACHEPROG", program)
        .env("GOTOOLCHAIN", "local")
        .env("GOFLAGS", "")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "go build failed:\n{text}");
    text
}

#[test]
fn live_go_builds_from_the_store_with_an_empty_gocache() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("server");

    let first = build(dir.path(), &store, "first");
    let second = build(dir.path(), &store, "second");

    assert!(
        first.contains("example.com/fixture"),
        "the first build compiled nothing:\n{first}"
    );
    assert!(
        !second.contains("example.com/fixture"),
        "the second build recompiled the fixture:\n{second}"
    );
}
