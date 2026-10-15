//! Unit tests for the Nx front-end.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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

async fn send(state: &Arc<State>, method: Method, path: &str, body: &[u8], auth: Option<&str>) -> (StatusCode, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(auth) = auth {
        request = request.header(header::AUTHORIZATION, auth);
    }
    let request = request.body(Full::new(Bytes::copy_from_slice(body))).unwrap();
    let response = route::handle(state.clone(), request).await.unwrap();
    let status = response.status();
    (status, response.into_body().collect().await.unwrap().to_bytes().to_vec())
}

#[test]
fn paths_parse_with_an_optional_tenant() {
    assert_eq!(
        parse("/v1/cache/12345"),
        Some(Target { tenant: String::new(), hash: "12345".into() })
    );
    assert_eq!(parse("/team/a/v1/cache/abc").unwrap().tenant, "team/a");
    assert_eq!(parse("/v1/cache/"), None);
    assert_eq!(parse("/v1/cache/bad/hash"), None);
    assert_eq!(parse("/v8/artifacts/abc"), None);
}

#[tokio::test]
async fn a_task_output_round_trips() {
    let (_dir, state) = state_with(Access::open());

    assert_eq!(send(&state, Method::PUT, "/v1/cache/987", b"tar", None).await.0, StatusCode::ACCEPTED);
    assert_eq!(send(&state, Method::GET, "/v1/cache/987", b"", None).await, (StatusCode::OK, b"tar".to_vec()));
}

#[tokio::test]
async fn records_are_immutable() {
    let (_dir, state) = state_with(Access::open());
    send(&state, Method::PUT, "/v1/cache/987", b"first", None).await;

    let (status, body) = send(&state, Method::PUT, "/v1/cache/987", b"second", None).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body, b"Cannot override an existing record\n");
    assert_eq!(send(&state, Method::GET, "/v1/cache/987", b"", None).await.1, b"first");
}

#[tokio::test]
async fn a_miss_is_not_found() {
    let (_dir, state) = state_with(Access::open());

    let (status, body) = send(&state, Method::GET, "/v1/cache/404", b"", None).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, b"The record was not found\n");
}

#[tokio::test]
async fn tenants_are_separate() {
    let (_dir, state) = state_with(Access::open());
    send(&state, Method::PUT, "/a/v1/cache/1", b"a", None).await;

    assert_eq!(send(&state, Method::GET, "/b/v1/cache/1", b"", None).await.0, StatusCode::NOT_FOUND);
    // The same hash may be written once per tenant.
    assert_eq!(send(&state, Method::PUT, "/b/v1/cache/1", b"b", None).await.0, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn a_read_only_token_cannot_write() {
    let tokens = Tokens::parse("ci nx:write\nlocal nx:read\n").unwrap();
    let (_dir, state) = state_with(Access::new(tokens, false));

    assert_eq!(send(&state, Method::PUT, "/v1/cache/1", b"x", None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&state, Method::PUT, "/v1/cache/1", b"x", Some("Bearer local")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(send(&state, Method::PUT, "/v1/cache/1", b"x", Some("Bearer ci")).await.0, StatusCode::ACCEPTED);
    assert_eq!(send(&state, Method::GET, "/v1/cache/1", b"", Some("Bearer local")).await.0, StatusCode::OK);
}

#[tokio::test]
async fn oversized_outputs_and_other_methods_are_refused() {
    let (_dir, state) = state_with(Access::open());

    assert_eq!(send(&state, Method::PUT, "/v1/cache/1", &[0; 2048], None).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(send(&state, Method::GET, "/v1/cache/1", b"", None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(send(&state, Method::DELETE, "/v1/cache/1", b"", None).await.0, StatusCode::METHOD_NOT_ALLOWED);
}
