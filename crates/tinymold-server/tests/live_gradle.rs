//! Live test: Gradle's HTTP build cache against the server, with basic
//! auth. Runs only with `TINYMOLD_LIVE=1` and `gradle` on `PATH`; it builds
//! a copy of `tests/fixtures/gradle`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use std::process::Command;

fn compile(project: &Path, cache: &str) -> String {
    common::run(
        Command::new("gradle")
            .args(["clean", "compileJava", "--build-cache", "--no-daemon", "--console=plain"])
            .arg("--info")
            .current_dir(project)
            .env("TINYMOLD_GRADLE_CACHE", cache)
            .env("TINYMOLD_GRADLE_TOKEN", "gradle-token"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn live_gradle_loads_compiled_classes_from_the_server() {
    if !common::live("gradle") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let server = common::start(dir.path(), Some("gradle-token gradle:write\n"));
    let cache = format!("http://{}/gradle/", server.addr);
    let project = dir.path().join("project");

    let (first, second) = tokio::task::spawn_blocking(move || {
        common::copy_tree(&common::fixture("gradle"), &project);
        let first = compile(&project, &cache);
        let second = compile(&project, &cache);
        (first, second)
    })
    .await
    .unwrap();

    assert!(first.contains("Stored cache entry for task ':compileJava'"), "the first build stored nothing:\n{first}");
    assert!(second.contains("Loaded cache entry for task ':compileJava'"), "the second build missed the server:\n{second}");
}
