//! Helpers the live tests share: whether to run, fixtures, and a server on
//! a loopback port.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

use tinymold_server::{Config, Server};
use tokio::sync::oneshot;

/// Whether live tests are enabled (`TINYMOLD_LIVE=1`) and `tool` runs.
pub(crate) fn live(tool: &str) -> bool {
    std::env::var_os("TINYMOLD_LIVE").is_some_and(|value| value == "1")
        && Command::new(tool).arg("--version").output().is_ok()
}

/// A fixture directory under `tests/fixtures`.
pub(crate) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Copies the tree at `from` to `to`.
pub(crate) fn copy_tree(from: &Path, to: &Path) {
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

/// Copies a fixture into `to` as a git repository that ignores build
/// output, as tools that hash what git sees expect. The fixture's
/// `gitignore` becomes `.gitignore`; it is not named that in this
/// repository so it does not apply here.
#[allow(dead_code, reason = "the Gradle test builds outside git")]
pub(crate) fn repository(name: &str, to: &Path) {
    copy_tree(&fixture(name), to);
    let ignore = to.join("gitignore");
    if ignore.exists() {
        std::fs::rename(ignore, to.join(".gitignore")).unwrap();
    }
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(to)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");
}

/// A server running on a loopback port until dropped.
pub(crate) struct Running {
    pub(crate) addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Starts a server storing under `dir`, with `tokens` as its token file
/// when given. Must be called inside a multi-threaded tokio runtime.
pub(crate) fn start(dir: &Path, tokens: Option<&str>) -> Running {
    let mut config = Config::new(dir.join("store"));
    config.listen = "127.0.0.1:0".parse().unwrap();
    if let Some(tokens) = tokens {
        let path = dir.join("tokens");
        std::fs::write(&path, tokens).unwrap();
        config.tokens = Some(path);
    }
    let server = Server::bind(&config).unwrap();
    let addr = server.local_addr();
    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    Running {
        addr,
        stop: Some(stop),
    }
}

/// Runs `command` and returns its combined output, failing the test when
/// it fails.
pub(crate) fn run(command: &mut Command) -> String {
    let output = command.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{command:?} failed:\n{text}");
    text
}
