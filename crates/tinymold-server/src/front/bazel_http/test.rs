//! Unit tests for the Bazel HTTP front-end.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use http::header;
use http_body_util::{BodyExt as _, Full};
use tinymold_store::Store;

use super::*;
use crate::auth::{Access, Tokens};
use crate::metrics::Metrics;

fn state_with(access: Access) -> (tempfile::TempDir, Arc<State>) {
    let dir = tempfile::tempdir().unwrap();
    let state = State {
        store: Store::open(dir.path().join("store")).unwrap(),
        access,
        metrics: Metrics::default(),
        max_body: 1024,
    };
    (dir, Arc::new(state))
}

fn state() -> (tempfile::TempDir, Arc<State>) {
    state_with(Access::open())
}

async fn send(
    state: &Arc<State>,
    method: Method,
    path: &str,
    body: &[u8],
    auth: Option<&str>,
) -> (StatusCode, http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    let request = builder
        .body(Full::new(Bytes::copy_from_slice(body)))
        .unwrap();
    let response = route::handle(state.clone(), request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, body.to_vec())
}

fn cas(bytes: &[u8]) -> String {
    format!("/cas/{}", Sha256::of_bytes(bytes))
}

#[test]
fn paths_parse_with_and_without_a_tenant() {
    let digest = Sha256::of_bytes(b"x");

    let plain = parse(&format!("/ac/{digest}")).unwrap();
    assert_eq!((plain.tenant.as_str(), plain.kind), ("", Kind::ActionCache));

    let nested = parse(&format!("/team/ccache/cas/{digest}")).unwrap();
    assert_eq!((nested.tenant.as_str(), nested.kind), ("team/ccache", Kind::Cas));
    assert_eq!(nested.digest, digest);

    assert_eq!(parse("/cas/not-a-digest"), None);
    assert_eq!(parse(&format!("/blobs/{digest}")), None);
    assert_eq!(parse("/healthz"), None);
}

#[tokio::test]
async fn a_blob_round_trips_through_cas() {
    let (_dir, state) = state();
    let path = cas(b"blob bytes");

    assert_eq!(send(&state, Method::PUT, &path, b"blob bytes", None).await.0, StatusCode::OK);
    let (status, headers, body) = send(&state, Method::GET, &path, b"", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"blob bytes");
    assert_eq!(headers[header::CONTENT_LENGTH], "10");
}

#[tokio::test]
async fn head_reports_the_length_without_a_body() {
    let (_dir, state) = state();
    let path = cas(b"blob bytes");
    send(&state, Method::PUT, &path, b"blob bytes", None).await;

    let (status, headers, body) = send(&state, Method::HEAD, &path, b"", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_LENGTH], "10");
    assert!(body.is_empty());
}

#[tokio::test]
async fn a_cas_body_that_does_not_match_its_name_is_refused_and_not_stored() {
    let (_dir, state) = state();
    let path = cas(b"what the name says");

    let (status, _, _) = send(&state, Method::PUT, &path, b"something else", None).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(state.store.cas().objects().unwrap().is_empty());
}

#[tokio::test]
async fn an_action_result_round_trips_and_is_scoped_to_its_tenant() {
    let (_dir, state) = state();
    let digest = Sha256::of_bytes(b"action");
    let path = format!("/team-a/ac/{digest}");

    send(&state, Method::PUT, &path, b"opaque result", None).await;

    assert_eq!(send(&state, Method::GET, &path, b"", None).await.2, b"opaque result");
    let other = format!("/team-b/ac/{digest}");
    assert_eq!(send(&state, Method::GET, &other, b"", None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_action_value_is_also_a_cas_blob() {
    let (_dir, state) = state();
    let digest = Sha256::of_bytes(b"action");
    send(&state, Method::PUT, &format!("/ac/{digest}"), b"value", None).await;

    let (status, _, body) = send(&state, Method::GET, &cas(b"value"), b"", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"value");
}

#[tokio::test]
async fn a_miss_is_not_found_and_counted() {
    let (_dir, state) = state();

    assert_eq!(send(&state, Method::GET, &cas(b"absent"), b"", None).await.0, StatusCode::NOT_FOUND);
    assert!(state.metrics.render().contains("tinymold_misses_total{front_end=\"bazel-http\"} 1"));
}

#[tokio::test]
async fn bodies_over_the_limit_are_refused() {
    let (_dir, state) = state();
    let big = vec![7_u8; 2048];
    let path = cas(&big);

    // Declared too large: refused before reading.
    let request = Request::builder()
        .method(Method::PUT)
        .uri(&path)
        .header(header::CONTENT_LENGTH, "2048")
        .body(Full::new(Bytes::from(big.clone())))
        .unwrap();
    let response = route::handle(state.clone(), request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // Undeclared: refused while streaming.
    assert_eq!(send(&state, Method::PUT, &path, &big, None).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(state.store.cas().objects().unwrap().is_empty());
}

#[tokio::test]
async fn other_methods_are_not_allowed() {
    let (_dir, state) = state();

    assert_eq!(
        send(&state, Method::DELETE, &cas(b"x"), b"", None).await.0,
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn credentials_are_checked() {
    let tokens = Tokens::parse("writer *:write\nreader bazel-http:read\n").unwrap();
    let (_dir, state) = state_with(Access::new(tokens, false));
    let path = cas(b"secret build output");

    let (status, headers, _) = send(&state, Method::PUT, &path, b"secret build output", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers[header::WWW_AUTHENTICATE].to_str().unwrap().starts_with("Basic"));

    let read_only = Some("Bearer reader");
    let status = send(&state, Method::PUT, &path, b"secret build output", read_only).await.0;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let writer = Some("Bearer writer");
    let status = send(&state, Method::PUT, &path, b"secret build output", writer).await.0;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(send(&state, Method::GET, &path, b"", read_only).await.0, StatusCode::OK);

    let metrics = state.metrics.render();
    assert!(metrics.contains("tinymold_denied_total{front_end=\"bazel-http\"} 2"), "{metrics}");
    assert!(!metrics.contains("writer"));
}
