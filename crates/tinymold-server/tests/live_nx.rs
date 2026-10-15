//! Live test: Nx's self-hosted remote cache against the server. Runs only
//! with `TINYMOLD_LIVE=1` and `npm` on `PATH`; it installs `nx` from npm into
//! a copy of `tests/fixtures/nx`, and needs `git`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use std::process::Command;

fn build(repo: &Path, server: &str) -> String {
    common::run(
        Command::new("npx")
            .args([
                "--no-install",
                "nx",
                "run-many",
                "-t",
                "build",
                "--outputStyle=static",
            ])
            .current_dir(repo)
            .env("NX_SELF_HOSTED_REMOTE_CACHE_SERVER", server)
            .env("NX_SELF_HOSTED_REMOTE_CACHE_ACCESS_TOKEN", "nx-token")
            .env("NX_DAEMON", "false")
            .env("NX_NO_CLOUD", "true")
            .env("NX_TUI", "false"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn live_nx_restores_a_task_from_the_server() {
    if !common::live("npm") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let server = common::start(dir.path(), Some("nx-token nx:write\n"));
    let url = format!("http://{}", server.addr);
    let repo = dir.path().join("repo");

    let (first, second, restored) = tokio::task::spawn_blocking(move || {
        common::repository("nx", &repo);
        common::run(
            Command::new("npm")
                .args(["install", "--no-audit", "--no-fund", "--loglevel=error"])
                .current_dir(&repo),
        );
        let first = build(&repo, &url);
        std::fs::remove_dir_all(repo.join("packages/a/dist")).unwrap();
        std::fs::remove_dir_all(repo.join(".nx")).unwrap();
        let second = build(&repo, &url);
        let restored = std::fs::read_to_string(repo.join("packages/a/dist/out.txt")).unwrap();
        (first, second, restored)
    })
    .await
    .unwrap();

    assert!(
        !first.contains("remote cache"),
        "the first run had nothing to restore:\n{first}"
    );
    assert!(
        second.contains("[remote cache]"),
        "the second run missed the server:\n{second}"
    );
    assert_eq!(restored, "built a");
}
