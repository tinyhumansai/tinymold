//! Live test: Turborepo's remote cache against the server. Runs only with
//! `TINYMOLD_LIVE=1` and `npm` on `PATH`; it installs `turbo` from npm into
//! a copy of `tests/fixtures/turbo`, and needs `git`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use std::process::Command;

/// Runs `turbo run build` against the server with only the remote cache,
/// and returns its output.
fn build(repo: &Path, api: &str) -> String {
    common::run(
        Command::new("npx")
            .args(["--no-install", "turbo", "run", "build", "--cache=remote:rw"])
            .current_dir(repo)
            .env("TURBO_API", api)
            .env("TURBO_TOKEN", "unused-on-an-open-server")
            .env("TURBO_TEAM", "fixture")
            .env("TURBO_TELEMETRY_DISABLED", "1")
            .env("DO_NOT_TRACK", "1"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn live_turbo_replays_every_task_from_the_server() {
    if !common::live("npm") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let server = common::start(dir.path(), None);
    let api = format!("http://{}", server.addr);
    let repo = dir.path().join("repo");

    let (first, second, restored) = tokio::task::spawn_blocking(move || {
        common::repository("turbo", &repo);
        common::run(
            Command::new("npm")
                .args(["install", "--no-audit", "--no-fund", "--loglevel=error"])
                .current_dir(&repo),
        );
        let first = build(&repo, &api);
        // Nothing local survives: only the server can answer.
        for package in ["a", "b"] {
            let package = repo.join("packages").join(package);
            std::fs::remove_dir_all(package.join("dist")).unwrap();
            let _ = std::fs::remove_dir_all(package.join(".turbo"));
        }
        let _ = std::fs::remove_dir_all(repo.join(".turbo"));
        let second = build(&repo, &api);
        let restored = std::fs::read_to_string(repo.join("packages/b/dist/out.txt")).unwrap();
        (first, second, restored)
    })
    .await
    .unwrap();

    assert!(!first.contains("FULL TURBO"), "the first run had nothing to replay:\n{first}");
    assert!(second.contains("FULL TURBO"), "the second run missed the server:\n{second}");
    assert_eq!(restored, "built b");
}
