//! Gradle's HTTP build cache.
//!
//! ```text
//! GET|HEAD  /gradle/[<tenant>/]<key>   200 with the cache entry, or 404
//! PUT       /gradle/[<tenant>/]<key>   200, or 413 over the size limit
//! ```
//!
//! Point Gradle's `HttpBuildCache` at `http://host:9090/gradle/` (or a
//! deeper path, which becomes the tenant). Keys are Gradle's hexadecimal
//! cache keys. Gradle sends basic-auth credentials, whose password is the
//! token, and may send `Expect: 100-continue`; an upload declared too large
//! is refused with `413` before its body is read, which Gradle treats as an
//! entry not worth caching rather than a failure.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use tinymold_store::{FrontEnd, MAX_CLIENT_KEY, Namespace};

use crate::auth::Permission;
use crate::front::{self, artifact, declared_length};
use crate::metrics::Outcome;
use crate::route::{self, Body, State};
use crate::transfer::UploadError;

/// The path every Gradle request starts with.
const PREFIX: &str = "/gradle/";

/// A parsed Gradle path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) tenant: String,
    pub(crate) key: String,
}

/// Parses `/gradle/[<tenant>/]<key>`, or `None` when `path` is not a
/// Gradle path.
pub(crate) fn parse(path: &str) -> Option<Target> {
    let rest = path.strip_prefix(PREFIX)?;
    let (tenant, key) = rest.rsplit_once('/').unwrap_or(("", rest));
    let valid = !key.is_empty()
        && key.len() <= MAX_CLIENT_KEY
        && key.bytes().all(|byte| byte.is_ascii_hexdigit());
    valid.then(|| Target {
        tenant: tenant.to_owned(),
        key: key.to_ascii_lowercase(),
    })
}

/// Answers one Gradle request.
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
    let Ok(namespace) = Namespace::new(FrontEnd::Gradle, target.tenant) else {
        return route::text(StatusCode::BAD_REQUEST, "tenant too long\n");
    };
    if let Err(denial) = state.access.check(request.headers(), &namespace, need) {
        record(state, Outcome::Denied, 0, 0);
        return front::denied(denial);
    }
    let Ok(key) = namespace.key(target.key.as_bytes()) else {
        return route::text(StatusCode::BAD_REQUEST, "key too long\n");
    };

    if need == Permission::Read {
        let head = request.method() == Method::HEAD;
        let Some(found) = artifact::find(state, key).await else {
            return miss(state);
        };
        let len = found.opened.len();
        let body = found.opened.into_body();
        record(state, Outcome::Hit, 0, if head { 0 } else { len });
        let mut response = route::sized(body, len, head);
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.gradle.build-cache-artifact.v2"),
        );
        return response;
    }

    if declared_length(&request).is_some_and(|len| len > state.max_body) {
        record(state, Outcome::Other, 0, 0);
        return route::text(StatusCode::PAYLOAD_TOO_LARGE, "cache entry too large\n");
    }
    let (result, received) = artifact::put(state, request.into_body(), key, BTreeMap::new()).await;
    match result {
        Ok(()) => {
            record(state, Outcome::Write, received, 0);
            route::empty(StatusCode::OK)
        }
        Err(error) => {
            record(state, Outcome::Other, received, 0);
            match error {
                UploadError::TooLarge => {
                    route::text(StatusCode::PAYLOAD_TOO_LARGE, "cache entry too large\n")
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
    route::text(StatusCode::NOT_FOUND, "not found\n")
}

fn record(state: &State, outcome: Outcome, bytes_in: u64, bytes_out: u64) {
    state
        .metrics
        .record(FrontEnd::Gradle, outcome, bytes_in, bytes_out);
}

#[cfg(test)]
mod test;
