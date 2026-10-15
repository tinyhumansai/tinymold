//! Nx's self-hosted remote cache API (Nx 20.8 and later).
//!
//! ```text
//! GET  [/<tenant>]/v1/cache/{hash}   200 with the task output, or 404
//! PUT  [/<tenant>]/v1/cache/{hash}   202, or 409 when the hash is stored
//! ```
//!
//! Clients set `NX_SELF_HOSTED_REMOTE_CACHE_SERVER` to the server's URL,
//! and any path in it is the tenant here, and send
//! `NX_SELF_HOSTED_REMOTE_CACHE_ACCESS_TOKEN` as a bearer token. Records are
//! immutable, as Nx's specification requires: a `PUT` to a stored hash is
//! refused with `409` before its body is read. Errors are plain text.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use tinymold_store::{FrontEnd, MAX_CLIENT_KEY, Namespace};

use crate::auth::Permission;
use crate::front::{self, artifact, declared_length};
use crate::metrics::Outcome;
use crate::route::{self, Body, State};
use crate::transfer::{self, UploadError};

/// A parsed Nx path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) tenant: String,
    pub(crate) hash: String,
}

/// Parses `[/<tenant>]/v1/cache/<hash>`, or `None` when `path` is not an
/// Nx path.
pub(crate) fn parse(path: &str) -> Option<Target> {
    let (prefix, hash) = path.rsplit_once("/v1/cache/")?;
    let valid = !hash.is_empty()
        && hash.len() <= MAX_CLIENT_KEY
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    valid.then(|| Target {
        tenant: prefix.trim_matches('/').to_owned(),
        hash: hash.to_owned(),
    })
}

/// Answers one Nx request.
pub(crate) async fn handle<B>(
    state: &Arc<State>,
    request: Request<B>,
    target: Target,
) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let need = match *request.method() {
        Method::GET | Method::HEAD => Permission::Read,
        Method::PUT => Permission::Write,
        _ => return route::text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n"),
    };
    let Ok(namespace) = Namespace::new(FrontEnd::Nx, target.tenant) else {
        return route::text(StatusCode::BAD_REQUEST, "tenant too long\n");
    };
    if let Err(denial) = state.access.check(request.headers(), &namespace, need) {
        record(state, Outcome::Denied, 0, 0);
        return front::denied(denial);
    }
    let Ok(key) = namespace.key(target.hash.as_bytes()) else {
        return route::text(StatusCode::BAD_REQUEST, "hash too long\n");
    };

    if need == Permission::Read {
        let head = request.method() == Method::HEAD;
        let Some(found) = artifact::find(state, key).await else {
            return miss(state);
        };
        let Some((body, len)) = transfer::download(found.path).await else {
            return miss(state);
        };
        record(state, Outcome::Hit, 0, if head { 0 } else { len });
        let mut response = route::sized(body, len, head);
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        return response;
    }

    if artifact::exists(state, key).await {
        record(state, Outcome::Other, 0, 0);
        return route::text(StatusCode::CONFLICT, "Cannot override an existing record\n");
    }
    if declared_length(&request).is_some_and(|len| len > state.max_body) {
        record(state, Outcome::Other, 0, 0);
        return route::text(StatusCode::PAYLOAD_TOO_LARGE, "task output too large\n");
    }
    let (result, received) = artifact::put(state, request.into_body(), key, BTreeMap::new()).await;
    match result {
        Ok(()) => {
            record(state, Outcome::Write, received, 0);
            route::empty(StatusCode::ACCEPTED)
        }
        Err(error) => {
            record(state, Outcome::Other, received, 0);
            match error {
                UploadError::TooLarge => {
                    route::text(StatusCode::PAYLOAD_TOO_LARGE, "task output too large\n")
                }
                UploadError::Body => route::text(StatusCode::BAD_REQUEST, "upload failed\n"),
                UploadError::Store(_) => {
                    route::text(StatusCode::INTERNAL_SERVER_ERROR, "store failed\n")
                }
            }
        }
    }
}

fn miss(state: &State) -> Response<Body> {
    record(state, Outcome::Miss, 0, 0);
    route::text(StatusCode::NOT_FOUND, "The record was not found\n")
}

fn record(state: &State, outcome: Outcome, bytes_in: u64, bytes_out: u64) {
    state
        .metrics
        .record(FrontEnd::Nx, outcome, bytes_in, bytes_out);
}

#[cfg(test)]
mod test;
