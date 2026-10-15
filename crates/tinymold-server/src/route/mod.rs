//! Request routing: which front-end answers which request.
//!
//! Every request is timed into the latency histogram. Paths no front-end
//! claims answer `404`, so a client pointed at the wrong prefix sees a
//! miss-shaped failure rather than another protocol's data.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode, header};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt as _, Empty, Full};
use tinymold_store::Store;

use crate::auth::Access;
use crate::front::{bazel_http, gradle, nx, reapi, turbo};
use crate::metrics::Metrics;

/// The body of every response: streamed, with I/O errors.
pub(crate) type Body = UnsyncBoxBody<Bytes, std::io::Error>;

/// What every request handler shares.
#[derive(Debug)]
pub(crate) struct State {
    pub(crate) store: Store,
    pub(crate) access: Access,
    pub(crate) metrics: Metrics,
    /// The largest request body accepted, in bytes.
    pub(crate) max_body: u64,
}

/// Answers one request. Never fails: every problem becomes a status code.
pub(crate) async fn handle<B>(
    state: Arc<State>,
    request: Request<B>,
) -> Result<Response<Body>, Infallible>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let started = Instant::now();
    let response = route(&state, request).await;
    state.metrics.observe(started.elapsed());
    Ok(response)
}

async fn route<B>(state: &Arc<State>, request: Request<B>) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if reapi::is_grpc(&request) {
        return reapi::handle(state, request).await;
    }
    if turbo::matches(request.uri().path()) {
        return turbo::handle(state, request).await;
    }
    if let Some(target) = gradle::parse(request.uri().path()) {
        return gradle::handle(state, request, target).await;
    }
    if let Some(target) = nx::parse(request.uri().path()) {
        return nx::handle(state, request, target).await;
    }
    if let Some(target) = bazel_http::parse(request.uri().path()) {
        return bazel_http::handle(state, request, target).await;
    }
    match (request.method(), request.uri().path()) {
        (&Method::GET | &Method::HEAD, "/healthz") => text(StatusCode::OK, "ok\n"),
        (&Method::GET, "/metrics") => {
            let mut response = text(StatusCode::OK, state.metrics.render());
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                http::HeaderValue::from_static("text/plain; version=0.0.4"),
            );
            response
        }
        _ => text(StatusCode::NOT_FOUND, "not found\n"),
    }
}

/// A response with no body.
pub(crate) fn empty(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Empty::new().map_err(|never| match never {}).boxed_unsync());
    *response.status_mut() = status;
    response
}

/// A response carrying `body` of `len` bytes, or only its headers for a
/// `HEAD` request.
pub(crate) fn sized(body: Body, len: u64, head: bool) -> Response<Body> {
    let mut response = if head {
        empty(StatusCode::OK)
    } else {
        Response::new(body)
    };
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, http::HeaderValue::from(len));
    response
}

/// A plain-text response.
pub(crate) fn text(status: StatusCode, body: impl Into<Bytes>) -> Response<Body> {
    let mut response = Response::new(
        Full::new(body.into())
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod test;
