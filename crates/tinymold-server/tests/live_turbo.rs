//! Live test: Turborepo's remote cache against the server. Runs only with
//! `TINYMOLD_LIVE=1` and `npm` on `PATH`; it installs `turbo` from npm into
//! a copy of `tests/fixtures/turbo`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

use tinymold_server::{Config, Server};
use tokio::sync::oneshot;

fn live() -> bool {
    std::env::var_os("TINYMOLD_LIVE").is_some_and(|value| value == "1")
        && Command::new("npm").arg("--version").output().is_ok()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Runs `turbo run build` against the server with only the remote cache,
/// and returns its output.
fn build(repo: &Path, api: &str) -> String {
    let output = Command::new("npx")
        .args(["--no-install", "turbo", "run", "build", "--cache=remote:rw"])
        .current_dir(repo)
        .env("TURBO_API", api)
        .env("TURBO_TOKEN", "unused-on-an-open-server")
        .env("TURBO_TEAM", "fixture")
        .env("TURBO_TELEMETRY_DISABLED", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "turbo failed:\n{text}");
    text
}

#[tokio::test(flavor = "multi_thread")]
async fn live_turbo_replays_every_task_from_the_server() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::new(dir.path().join("store"));
    config.listen = "127.0.0.1:0".parse().unwrap();
    let server = Server::bind(&config).unwrap();
    let api = format!("http://{}", server.local_addr());
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    let repo = dir.path().join("repo");
    copy_tree(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/turbo"), &repo);
    let (first, second, restored) = tokio::task::spawn_blocking(move || {
        let installed = Command::new("npm")
            .args(["install", "--no-audit", "--no-fund", "--loglevel=error"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(installed.success(), "npm install failed");

        let first = build(&repo, &api);
        for package in ["a", "b"] {
            std::fs::remove_dir_all(repo.join("packages").join(package).join("dist")).unwrap();
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
    let _ = stop.send(());
    task.await.unwrap().unwrap();
}
