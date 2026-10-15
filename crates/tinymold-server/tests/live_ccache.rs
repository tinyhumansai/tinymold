//! Live test: ccache's HTTP remote storage, bazel layout, against the
//! server. Runs only with `TINYMOLD_LIVE=1` and `ccache` on `PATH`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

use tinymold_server::{Config, Server};
use tokio::sync::oneshot;

fn live() -> bool {
    std::env::var_os("TINYMOLD_LIVE").is_some_and(|value| value == "1")
        && Command::new("ccache").arg("--version").output().is_ok()
}

/// Compiles `source` through ccache with an empty local cache at
/// `cache_dir`, and returns ccache's remote read hits.
fn compile(source: &Path, cache_dir: &Path, remote: &str) -> u64 {
    let object = source.with_extension("o");
    let status = Command::new("ccache")
        .args(["cc", "-c"])
        .arg(source)
        .arg("-o")
        .arg(&object)
        .env("CCACHE_DIR", cache_dir)
        .env("CCACHE_REMOTE_STORAGE", remote)
        .status()
        .unwrap();
    assert!(status.success());
    let stats = Command::new("ccache")
        .arg("--print-stats")
        .env("CCACHE_DIR", cache_dir)
        .output()
        .unwrap();
    String::from_utf8(stats.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("remote_storage_read_hit\t"))
        .map_or(0, |value| value.trim().parse().unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn live_ccache_hits_the_server_from_an_empty_local_cache() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::new(dir.path().join("store"));
    config.listen = "127.0.0.1:0".parse().unwrap();
    let server = Server::bind(&config).unwrap();
    let remote = format!("http://{}/ccache|layout=bazel", server.local_addr());
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    let source = dir.path().join("unit.c");
    std::fs::write(&source, "int answer(void) { return 42; }\n").unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    let hits = tokio::task::spawn_blocking(move || {
        let cold = compile(&source, &first, &remote);
        let warm = compile(&source, &second, &remote);
        (cold, warm)
    })
    .await
    .unwrap();

    assert_eq!(hits, (0, 1), "(first build, second build) remote hits");
    let _ = stop.send(());
    task.await.unwrap().unwrap();
}
