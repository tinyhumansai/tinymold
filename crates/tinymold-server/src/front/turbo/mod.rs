//! Turborepo's remote cache API.
//!
//! ```text
//! GET   /v8/artifacts/status            {"status":"enabled"}
//! HEAD  /v8/artifacts/{hash}            200 with artifact headers, or 404
//! GET   /v8/artifacts/{hash}            the artifact, or 404
//! PUT   /v8/artifacts/{hash}            202 {"urls":[...]}
//! POST  /v8/artifacts                   {"hashes":[...]} → {hash: info | null}
//! POST  /v8/artifacts/events            200, events are not kept
//! ```
//!
//! The team (`teamId`, else `slug`, from the query string) is the tenant,
//! so two teams never see each other's artifacts. An artifact is opaque
//! bytes, Turbo's gzipped tarball of task outputs, stored as one object.
//! The headers Turbo sends with an upload (`x-artifact-duration`,
//! `x-artifact-tag`, `x-artifact-sha`, `x-artifact-dirty-hash`) are kept in
//! the entry's metadata and returned on reads; an artifact tag is the
//! client's signature, and the client verifies it.
//!
//! Errors use the spec's `{"code": ..., "message": ...}` body. The API is
//! described by Turborepo's [Remote Cache API](https://turborepo.dev/docs/openapi).

use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::BodyExt as _;
use tinymold_store::{Entry, FrontEnd, Namespace};

use crate::auth::{Denied, Permission};
use crate::front::{blocking, declared_length, now};
use crate::metrics::Outcome;
use crate::route::{self, Body, State};
use crate::transfer::{self, UploadError};

/// The path prefix every Turbo endpoint lives under.
const PREFIX: &str = "/v8/artifacts";

/// The artifact headers kept with an upload and returned on reads.
const KEPT: [&str; 4] = [
    "x-artifact-duration",
    "x-artifact-tag",
    "x-artifact-sha",
    "x-artifact-dirty-hash",
];

/// The longest value accepted for any kept header, as the spec limits
/// `x-artifact-tag`.
const MAX_HEADER: usize = 600;

/// The largest batch query body accepted.
const MAX_QUERY: u64 = 1 << 20;

/// Whether `path` is a Turbo endpoint.
pub(crate) fn matches(path: &str) -> bool {
    path == PREFIX || path.starts_with("/v8/artifacts/")
}

/// Answers one Turbo request.
pub(crate) async fn handle<B>(state: &Arc<State>, request: Request<B>) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let team = team(request.uri().query().unwrap_or(""));
    let Ok(namespace) = Namespace::new(FrontEnd::Turbo, team) else {
        return error(StatusCode::BAD_REQUEST, "bad_request", "team too long");
    };
    let path = request.uri().path().to_owned();
    let rest = path.strip_prefix(PREFIX).unwrap_or("");
    let method = request.method().clone();

    let need = if method == Method::PUT || (method == Method::POST && rest == "/events") {
        Permission::Write
    } else {
        Permission::Read
    };
    if let Err(denial) = state.access.check(request.headers(), &namespace, need) {
        record(state, Outcome::Denied, 0, 0);
        return match denial {
            Denied::Unauthenticated => error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "a valid token is required",
            ),
            Denied::Forbidden => error(
                StatusCode::FORBIDDEN,
                "forbidden",
                "the token lacks this scope",
            ),
        };
    }

    match (method, rest) {
        (Method::GET, "/status") => json(StatusCode::OK, r#"{"status":"enabled"}"#.to_owned()),
        (Method::POST, "") => query(state, request, namespace).await,
        (Method::POST, "/events") => {
            record(state, Outcome::Other, 0, 0);
            route::empty(StatusCode::OK)
        }
        (method, hash) => match hash.strip_prefix('/').filter(|hash| is_hex(hash)) {
            Some(hash) => {
                let hash = hash.to_ascii_lowercase();
                match method {
                    Method::GET | Method::HEAD => {
                        download(state, &namespace, &hash, method == Method::HEAD).await
                    }
                    Method::PUT => upload(state, request, &namespace, &hash).await,
                    _ => error(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        "method not allowed",
                    ),
                }
            }
            None => error(StatusCode::NOT_FOUND, "not_found", "no such endpoint"),
        },
    }
}

async fn download(
    state: &Arc<State>,
    namespace: &Namespace,
    hash: &str,
    head: bool,
) -> Response<Body> {
    let store = state.store.clone();
    let key = namespace.key(hash.as_bytes());
    let found = blocking(move || {
        let entry = store.get(&key.ok()?).ok()??;
        let blob = *entry.blobs.first()?;
        Some((store.cas().object_path(&blob), entry.meta))
    })
    .await
    .flatten();

    let Some((path, meta)) = found else {
        return miss(state);
    };
    let Some((body, len)) = transfer::download(path).await else {
        return miss(state);
    };
    record(state, Outcome::Hit, 0, if head { 0 } else { len });
    let mut response = route::sized(body, len, head);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    for (name, value) in meta {
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            headers.insert(name, value);
        }
    }
    response
}

async fn upload<B>(
    state: &Arc<State>,
    request: Request<B>,
    namespace: &Namespace,
    hash: &str,
) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if declared_length(&request).is_some_and(|len| len > state.max_body) {
        record(state, Outcome::Other, 0, 0);
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "artifact too large",
        );
    }
    let meta = match kept_headers(request.headers()) {
        Ok(meta) => meta,
        Err(message) => return error(StatusCode::BAD_REQUEST, "bad_request", message),
    };
    let Ok(key) = namespace.key(hash.as_bytes()) else {
        return error(StatusCode::BAD_REQUEST, "bad_request", "hash too long");
    };
    let store = state.store.clone();
    let (result, received) = transfer::upload(request.into_body(), state.max_body, move |body| {
        let ingested = store.ingest(body, None)?;
        let mut entry = Entry::new(vec![ingested.digest]);
        entry.meta = meta;
        entry.written = now();
        store.put(&key, &entry)
    })
    .await;

    match result {
        Ok(()) => {
            record(state, Outcome::Write, received, 0);
            json(
                StatusCode::ACCEPTED,
                format!(r#"{{"urls":["{PREFIX}/{hash}"]}}"#),
            )
        }
        Err(UploadError::TooLarge) => {
            record(state, Outcome::Other, received, 0);
            error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "artifact too large",
            )
        }
        Err(UploadError::Body) => {
            record(state, Outcome::Other, received, 0);
            error(StatusCode::BAD_REQUEST, "bad_request", "upload failed")
        }
        Err(UploadError::Store(_)) => {
            record(state, Outcome::Other, received, 0);
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "store failed",
            )
        }
    }
}

async fn query<B>(state: &Arc<State>, request: Request<B>, namespace: Namespace) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let Ok(body) = http_body_util::Limited::new(
        request.into_body(),
        usize::try_from(MAX_QUERY).unwrap_or(usize::MAX),
    )
    .collect()
    .await
    else {
        return error(StatusCode::BAD_REQUEST, "bad_request", "unreadable query");
    };
    let Some(hashes) = parse_hashes(&body.to_bytes()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "expected {\"hashes\": [...]}",
        );
    };
    let store = state.store.clone();
    let answers = blocking(move || {
        hashes
            .into_iter()
            .map(|hash| {
                let info = namespace
                    .key(hash.to_ascii_lowercase().as_bytes())
                    .ok()
                    .and_then(|key| store.get(&key).ok().flatten())
                    .and_then(|entry| {
                        let blob = entry.blobs.first()?;
                        let size = std::fs::metadata(store.cas().object_path(blob)).ok()?.len();
                        Some((size, entry.meta))
                    });
                (hash, info)
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();

    let mut out = String::from("{");
    for (index, (hash, info)) in answers.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{hash}\":"));
        match info {
            None => out.push_str("null"),
            Some((size, meta)) => {
                let duration = meta
                    .get("x-artifact-duration")
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0);
                out.push_str(&format!("{{\"size\":{size},\"taskDurationMs\":{duration}"));
                if let Some(tag) = meta.get("x-artifact-tag") {
                    out.push_str(&format!(",\"tag\":{}", json_string(tag)));
                }
                out.push('}');
            }
        }
    }
    out.push('}');
    record(state, Outcome::Other, 0, 0);
    json(StatusCode::OK, out)
}

/// The kept artifact headers of an upload, or why one is refused.
fn kept_headers(
    headers: &HeaderMap,
) -> Result<std::collections::BTreeMap<String, String>, &'static str> {
    let mut meta = std::collections::BTreeMap::new();
    for name in KEPT {
        let Some(value) = headers.get(name) else {
            continue;
        };
        let value = value
            .to_str()
            .map_err(|_| "artifact headers must be ascii")?;
        if value.len() > MAX_HEADER {
            return Err("an artifact header is too long");
        }
        if name == "x-artifact-duration" && value.parse::<u64>().is_err() {
            return Err("x-artifact-duration must be a whole number of milliseconds");
        }
        meta.insert(name.to_owned(), value.to_owned());
    }
    Ok(meta)
}

/// The team a request names: `teamId`, else `slug`, else none.
fn team(query: &str) -> String {
    let mut slug = None;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        match name {
            "teamId" => return percent_decode(value),
            "slug" => slug = Some(percent_decode(value)),
            _ => {}
        }
    }
    slug.unwrap_or_default()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let escaped = (byte == b'%')
            .then(|| text.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (byte, escaped) {
            (_, Some(decoded)) => {
                out.push(decoded);
                index += 3;
            }
            (b'+', None) => {
                out.push(b' ');
                index += 1;
            }
            (byte, None) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The hex strings of `{"hashes": [...]}`, or `None` when the body is not
/// that shape.
fn parse_hashes(body: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(body).ok()?;
    let rest = text.trim_start().strip_prefix('{')?;
    let rest = rest.trim_start().strip_prefix("\"hashes\"")?;
    let rest = rest.trim_start().strip_prefix(':')?;
    let mut rest = rest.trim_start().strip_prefix('[')?;
    let mut hashes = Vec::new();
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix(']') {
            return after
                .trim_start()
                .strip_prefix('}')
                .map(|tail| tail.trim().is_empty())?
                .then_some(hashes);
        }
        if !hashes.is_empty() {
            rest = rest.strip_prefix(',')?.trim_start();
        }
        let inner = rest.strip_prefix('"')?;
        let end = inner.find('"')?;
        let hash = &inner[..end];
        if !is_hex(hash) {
            return None;
        }
        hashes.push(hash.to_owned());
        rest = &inner[end + 1..];
    }
}

/// `text` as a JSON string literal.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            control if control.is_control() => {
                out.push_str(&format!("\\u{:04x}", u32::from(control)))
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn json(status: StatusCode, body: String) -> Response<Body> {
    let mut response = route::text(status, body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn error(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    json(
        status,
        format!(
            r#"{{"code":{},"message":{}}}"#,
            json_string(code),
            json_string(message)
        ),
    )
}

fn miss(state: &State) -> Response<Body> {
    record(state, Outcome::Miss, 0, 0);
    error(StatusCode::NOT_FOUND, "not_found", "artifact not found")
}

fn record(state: &State, outcome: Outcome, bytes_in: u64, bytes_out: u64) {
    state
        .metrics
        .record(FrontEnd::Turbo, outcome, bytes_in, bytes_out);
}

#[cfg(test)]
mod test;
