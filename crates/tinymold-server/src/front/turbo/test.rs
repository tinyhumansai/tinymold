//! Unit tests for the Turborepo front-end.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use http_body_util::Full;
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

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn text(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap()
    }
}

async fn send(state: &Arc<State>, request: http::request::Builder, body: &[u8]) -> Reply {
    let request = request.body(Full::new(Bytes::copy_from_slice(body))).unwrap();
    let response = route::handle(state.clone(), request).await.unwrap();
    Reply {
        status: response.status(),
        headers: response.headers().clone(),
        body: response.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

fn put(path: &str) -> http::request::Builder {
    Request::builder().method(Method::PUT).uri(path)
}

fn get(path: &str) -> http::request::Builder {
    Request::builder().method(Method::GET).uri(path)
}

#[test]
fn teams_come_from_team_id_then_slug() {
    assert_eq!(team("teamId=team_1&slug=other"), "team_1");
    assert_eq!(team("slug=my%20team"), "my team");
    assert_eq!(team("slug=a+b&x=1"), "a b");
    assert_eq!(team(""), "");
}

#[test]
fn batch_queries_parse_only_hex_hash_lists() {
    assert_eq!(
        parse_hashes(br#" { "hashes" : [ "ab12", "CD" ] } "#),
        Some(vec!["ab12".to_owned(), "CD".to_owned()])
    );
    assert_eq!(parse_hashes(br#"{"hashes":[]}"#), Some(Vec::new()));
    assert_eq!(parse_hashes(br#"{"hashes":["zz"]}"#), None);
    assert_eq!(parse_hashes(br#"{"hashes":["ab"]} trailing"#), None);
    assert_eq!(parse_hashes(br#"{"other":[]}"#), None);
}

#[test]
fn json_strings_are_escaped() {
    assert_eq!(json_string("a\"b\\c\n"), r#""a\"b\\c\u000a""#);
}

#[tokio::test]
async fn status_reports_enabled() {
    let (_dir, state) = state();
    let reply = send(&state, get("/v8/artifacts/status?teamId=t"), b"").await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.text(), r#"{"status":"enabled"}"#);
}

#[tokio::test]
async fn an_artifact_round_trips_with_its_headers() {
    let (_dir, state) = state();
    let upload = put("/v8/artifacts/abc123?teamId=t")
        .header("x-artifact-duration", "1200")
        .header("x-artifact-tag", "c2lnbmF0dXJl")
        .header("x-artifact-client-ci", "github-actions");
    let stored = send(&state, upload, b"tarball bytes").await;
    assert_eq!(stored.status, StatusCode::ACCEPTED);
    assert_eq!(stored.text(), r#"{"urls":["/v8/artifacts/abc123"]}"#);

    let reply = send(&state, get("/v8/artifacts/ABC123?teamId=t"), b"").await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, b"tarball bytes");
    assert_eq!(reply.headers["x-artifact-duration"], "1200");
    assert_eq!(reply.headers["x-artifact-tag"], "c2lnbmF0dXJl");
    assert_eq!(reply.headers[header::CONTENT_TYPE], "application/octet-stream");
    assert!(!reply.headers.contains_key("x-artifact-client-ci"));
}

#[tokio::test]
async fn head_reports_existence_without_a_body() {
    let (_dir, state) = state();
    send(&state, put("/v8/artifacts/abc?slug=s"), b"tar").await;

    let present = send(&state, Request::builder().method(Method::HEAD).uri("/v8/artifacts/abc?slug=s"), b"").await;
    let absent = send(&state, Request::builder().method(Method::HEAD).uri("/v8/artifacts/def?slug=s"), b"").await;

    assert_eq!(present.status, StatusCode::OK);
    assert_eq!(present.headers[header::CONTENT_LENGTH], "3");
    assert_eq!(present.body, b"");
    assert_eq!(absent.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn teams_do_not_see_each_others_artifacts() {
    let (_dir, state) = state();
    send(&state, put("/v8/artifacts/abc?teamId=a"), b"team a").await;

    let other = send(&state, get("/v8/artifacts/abc?teamId=b"), b"").await;

    assert_eq!(other.status, StatusCode::NOT_FOUND);
    assert_eq!(other.text(), r#"{"code":"not_found","message":"artifact not found"}"#);
}

#[tokio::test]
async fn a_batch_query_reports_size_duration_and_tag_or_null() {
    let (_dir, state) = state();
    let upload = put("/v8/artifacts/aa?teamId=t")
        .header("x-artifact-duration", "77")
        .header("x-artifact-tag", "tag");
    send(&state, upload, b"12345").await;

    let reply = send(
        &state,
        Request::builder().method(Method::POST).uri("/v8/artifacts?teamId=t"),
        br#"{"hashes":["aa","bb"]}"#,
    )
    .await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.text(),
        r#"{"aa":{"size":5,"taskDurationMs":77,"tag":"tag"},"bb":null}"#
    );
}

#[tokio::test]
async fn malformed_requests_are_refused() {
    let (_dir, state) = state();
    let post = || Request::builder().method(Method::POST).uri("/v8/artifacts");

    assert_eq!(send(&state, post(), b"not json").await.status, StatusCode::BAD_REQUEST);
    assert_eq!(send(&state, get("/v8/artifacts/not-hex"), b"").await.status, StatusCode::NOT_FOUND);
    let bad_duration = put("/v8/artifacts/ab").header("x-artifact-duration", "soon");
    assert_eq!(send(&state, bad_duration, b"x").await.status, StatusCode::BAD_REQUEST);
    let long_tag = put("/v8/artifacts/ab").header("x-artifact-tag", "t".repeat(MAX_HEADER + 1));
    assert_eq!(send(&state, long_tag, b"x").await.status, StatusCode::BAD_REQUEST);
    let delete = Request::builder().method(Method::DELETE).uri("/v8/artifacts/ab");
    assert_eq!(send(&state, delete, b"").await.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn an_artifact_over_the_limit_is_refused() {
    let (_dir, state) = state();
    let big = vec![0_u8; 2048];

    assert_eq!(send(&state, put("/v8/artifacts/ab"), &big).await.status, StatusCode::PAYLOAD_TOO_LARGE);
    let declared = put("/v8/artifacts/ab").header(header::CONTENT_LENGTH, "2048");
    assert_eq!(send(&state, declared, &big).await.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn events_are_accepted() {
    let (_dir, state) = state();
    let events = Request::builder().method(Method::POST).uri("/v8/artifacts/events?teamId=t");

    assert_eq!(send(&state, events, br#"[{"event":"HIT"}]"#).await.status, StatusCode::OK);
}

#[tokio::test]
async fn bearer_tokens_are_checked_per_team() {
    let tokens = Tokens::parse("ci turbo:write\nteam-a turbo/a:read\n").unwrap();
    let (_dir, state) = state_with(Access::new(tokens, false));
    let bearer = |token: &str| format!("Bearer {token}");

    let anonymous = send(&state, put("/v8/artifacts/ab?teamId=a"), b"x").await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.text(), r#"{"code":"unauthorized","message":"a valid token is required"}"#);

    let reader = put("/v8/artifacts/ab?teamId=a").header(header::AUTHORIZATION, bearer("team-a"));
    assert_eq!(send(&state, reader, b"x").await.status, StatusCode::FORBIDDEN);

    let writer = put("/v8/artifacts/ab?teamId=a").header(header::AUTHORIZATION, bearer("ci"));
    assert_eq!(send(&state, writer, b"x").await.status, StatusCode::ACCEPTED);
    let read = get("/v8/artifacts/ab?teamId=a").header(header::AUTHORIZATION, bearer("team-a"));
    assert_eq!(send(&state, read, b"").await.status, StatusCode::OK);
    let other_team = get("/v8/artifacts/ab?teamId=b").header(header::AUTHORIZATION, bearer("team-a"));
    assert_eq!(send(&state, other_team, b"").await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_turbo_artifact_dedupes_with_other_protocols() {
    let (_dir, state) = state();
    send(&state, put("/v8/artifacts/ab"), b"shared bytes").await;
    let sha = tinymold_store::Sha256::of_bytes(b"shared bytes");

    let reply = send(&state, get(&format!("/cas/{sha}")), b"").await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(state.store.cas().objects().unwrap().len(), 1);
}
