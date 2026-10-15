//! Unit tests for the Gradle front-end.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use base64::Engine as _;
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

async fn send(
    state: &Arc<State>,
    request: http::request::Builder,
    body: &[u8],
) -> (StatusCode, http::HeaderMap, Vec<u8>) {
    let request = request
        .body(Full::new(Bytes::copy_from_slice(body)))
        .unwrap();
    let response = route::handle(state.clone(), request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    (
        status,
        headers,
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}

fn at(method: Method, path: &str) -> http::request::Builder {
    Request::builder().method(method).uri(path)
}

fn basic(user: &str, password: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    format!("Basic {encoded}")
}

#[test]
fn paths_parse_with_an_optional_tenant() {
    assert_eq!(
        parse("/gradle/ABCdef01"),
        Some(Target {
            tenant: String::new(),
            key: "abcdef01".into()
        })
    );
    assert_eq!(parse("/gradle/team/main/ab").unwrap().tenant, "team/main");
    assert_eq!(parse("/gradle/"), None);
    assert_eq!(parse("/gradle/not-hex"), None);
    assert_eq!(parse("/cas/ab"), None);
}

#[tokio::test]
async fn a_cache_entry_round_trips() {
    let (_dir, state) = state_with(Access::open());

    assert_eq!(
        send(&state, at(Method::PUT, "/gradle/ab12"), b"entry")
            .await
            .0,
        StatusCode::OK
    );
    let (status, headers, body) = send(&state, at(Method::GET, "/gradle/ab12"), b"").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"entry");
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "application/vnd.gradle.build-cache-artifact.v2"
    );
}

#[tokio::test]
async fn a_later_upload_replaces_an_entry() {
    let (_dir, state) = state_with(Access::open());
    send(&state, at(Method::PUT, "/gradle/ab"), b"first").await;
    send(&state, at(Method::PUT, "/gradle/ab"), b"second").await;

    assert_eq!(
        send(&state, at(Method::GET, "/gradle/ab"), b"").await.2,
        b"second"
    );
}

#[tokio::test]
async fn misses_and_tenants() {
    let (_dir, state) = state_with(Access::open());
    send(&state, at(Method::PUT, "/gradle/a/ab"), b"a").await;

    assert_eq!(
        send(&state, at(Method::GET, "/gradle/b/ab"), b"").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&state, at(Method::HEAD, "/gradle/a/ab"), b"").await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_entry_declared_too_large_is_refused_before_it_is_read() {
    let (_dir, state) = state_with(Access::open());
    let declared = at(Method::PUT, "/gradle/ab")
        .header(header::CONTENT_LENGTH, "4096")
        .header(header::EXPECT, "100-continue");

    assert_eq!(
        send(&state, declared, &[0; 4096]).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        send(&state, at(Method::PUT, "/gradle/ab"), &[0; 4096])
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(state.store.cas().objects().unwrap(), Vec::new());
}

#[tokio::test]
async fn basic_auth_carries_the_token_and_401_challenges() {
    let tokens = Tokens::parse("ci-token gradle:write\n").unwrap();
    let (_dir, state) = state_with(Access::new(tokens, false));

    let (status, headers, _) = send(&state, at(Method::PUT, "/gradle/ab"), b"x").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers.contains_key(header::WWW_AUTHENTICATE));

    let authorized =
        at(Method::PUT, "/gradle/ab").header(header::AUTHORIZATION, basic("gradle", "ci-token"));
    assert_eq!(send(&state, authorized, b"x").await.0, StatusCode::OK);
    assert_eq!(
        send(&state, at(Method::POST, "/gradle/ab"), b"").await.0,
        StatusCode::METHOD_NOT_ALLOWED
    );
}
