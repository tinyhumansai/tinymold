//! Unit tests for request routing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn state() -> (tempfile::TempDir, Arc<State>) {
    let dir = tempfile::tempdir().unwrap();
    let state = State {
        store: Store::open(dir.path().join("store")).unwrap(),
        access: Access::open(),
        metrics: Metrics::default(),
        max_body: 1024,
    };
    (dir, Arc::new(state))
}

async fn send(state: &Arc<State>, method: Method, path: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = handle(state.clone(), request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn health_answers_ok() {
    let (_dir, state) = state();

    assert_eq!(
        send(&state, Method::GET, "/healthz").await,
        (StatusCode::OK, "ok\n".into())
    );
}

#[tokio::test]
async fn metrics_render_and_count_latency() {
    let (_dir, state) = state();
    send(&state, Method::GET, "/healthz").await;
    let (status, body) = send(&state, Method::GET, "/metrics").await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("tinymold_request_duration_seconds_count 1"),
        "{body}"
    );
}

#[tokio::test]
async fn unknown_paths_are_not_found() {
    let (_dir, state) = state();

    assert_eq!(
        send(&state, Method::GET, "/v8/nothing").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&state, Method::POST, "/healthz").await.0,
        StatusCode::NOT_FOUND
    );
}
