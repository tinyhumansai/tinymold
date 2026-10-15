//! Bazel's HTTP cache protocol, also spoken by ccache.
//!
//! ```text
//! GET|HEAD|PUT  [/<tenant>]/cas/<sha256>   a blob, named by its SHA-256
//! GET|HEAD|PUT  [/<tenant>]/ac/<sha256>    an action result, by action digest
//! ```
//!
//! Bazel points `--remote_cache=http://host[/prefix]` at a server and uses
//! the prefix, if any, as its tenant here. ccache's HTTP storage with
//! `layout=bazel` stores every cache entry under `/ac/`.
//!
//! - A `/cas/` upload must hash to its name; one that does not is refused
//!   with `400` and not stored. A `/cas/` read finds the blob through the
//!   store's SHA-256 index, whichever protocol uploaded it.
//! - An `/ac/` value is opaque: it is stored as one object under the key
//!   `(bazel-http, tenant, digest)` and returned unchanged.
//!
//! Uploads answer `200`, misses `404`, bodies over the limit `413`.

use std::str::FromStr as _;
use std::sync::Arc;

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use tinymold_store::{Claim, Entry, FrontEnd, Namespace, Sha256};

use crate::auth::Permission;
use crate::front::{self, blocking, declared_length};
use crate::metrics::Outcome;
use crate::route::{self, Body, State};
use crate::transfer::{self, UploadError};

/// Which half of the cache a request addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `/ac/`: action results by action digest.
    ActionCache,
    /// `/cas/`: blobs by their own digest.
    Cas,
}

/// A parsed Bazel HTTP path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) tenant: String,
    pub(crate) kind: Kind,
    pub(crate) digest: Sha256,
}

/// Parses `[/<tenant>]/(ac|cas)/<sha256>`, or `None` when `path` is not a
/// Bazel HTTP path.
pub(crate) fn parse(path: &str) -> Option<Target> {
    let path = path.strip_prefix('/')?;
    let (rest, hex) = path.rsplit_once('/')?;
    let (tenant, kind) = match rest.rsplit_once('/') {
        Some((tenant, kind)) => (tenant, kind),
        None => ("", rest),
    };
    let kind = match kind {
        "ac" => Kind::ActionCache,
        "cas" => Kind::Cas,
        _ => return None,
    };
    let digest = Sha256::from_str(hex).ok()?;
    Some(Target {
        tenant: tenant.to_owned(),
        kind,
        digest,
    })
}

/// Answers one Bazel HTTP request.
pub(crate) async fn handle<B>(
    state: &Arc<State>,
    request: Request<B>,
    target: Target,
) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let Ok(namespace) = Namespace::new(FrontEnd::BazelHttp, target.tenant.clone()) else {
        return route::text(StatusCode::BAD_REQUEST, "tenant too long\n");
    };
    let need = match *request.method() {
        Method::GET | Method::HEAD => Permission::Read,
        Method::PUT => Permission::Write,
        _ => return route::text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n"),
    };
    if let Err(denial) = state.access.check(request.headers(), &namespace, need) {
        record(state, Outcome::Denied, 0, 0);
        return front::denied(denial);
    }

    if need == Permission::Write {
        return put(state, request, &namespace, &target).await;
    }
    let head = request.method() == Method::HEAD;
    let store = state.store.clone();
    let located = blocking(move || -> Option<std::path::PathBuf> {
        let digest = match target.kind {
            Kind::Cas => store.find_sha256(&target.digest).ok()??,
            Kind::ActionCache => {
                let key = namespace.key(target.digest.as_bytes()).ok()?;
                *store.get(&key).ok()??.blobs.first()?
            }
        };
        Some(store.cas().object_path(&digest))
    })
    .await
    .flatten();

    match located {
        Some(path) => match transfer::download(path).await {
            Some((body, len)) => {
                record(state, Outcome::Hit, 0, if head { 0 } else { len });
                route::sized(body, len, head)
            }
            // Collected between the lookup and the read: a miss.
            None => miss(state),
        },
        None => miss(state),
    }
}

async fn put<B>(
    state: &Arc<State>,
    request: Request<B>,
    namespace: &Namespace,
    target: &Target,
) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if declared_length(&request).is_some_and(|len| len > state.max_body) {
        record(state, Outcome::Other, 0, 0);
        return route::text(StatusCode::PAYLOAD_TOO_LARGE, "body too large\n");
    }
    let store = state.store.clone();
    let kind = target.kind;
    let digest = target.digest;
    let key = namespace.key(digest.as_bytes());
    let (result, received) = transfer::upload(request.into_body(), state.max_body, move |body| {
        let claim = (kind == Kind::Cas).then_some(Claim::Sha256(digest));
        let ingested = store.ingest(body, claim)?;
        if kind == Kind::ActionCache {
            store.put(&key?, &Entry::new(vec![ingested.digest]))?;
        }
        Ok::<_, tinymold_store::Error>(())
    })
    .await;

    match result {
        Ok(()) => {
            record(state, Outcome::Write, received, 0);
            route::empty(StatusCode::OK)
        }
        Err(error) => {
            record(state, Outcome::Other, received, 0);
            match error {
                UploadError::TooLarge => {
                    route::text(StatusCode::PAYLOAD_TOO_LARGE, "body too large\n")
                }
                UploadError::Store(tinymold_store::Error::DigestMismatch { .. }) => {
                    route::text(StatusCode::BAD_REQUEST, "body does not match its digest\n")
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
        .record(FrontEnd::BazelHttp, outcome, bytes_in, bytes_out);
}

#[cfg(test)]
mod test;
