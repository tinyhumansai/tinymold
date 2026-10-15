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
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Empty, Full};
use tinymold_store::Store;

use crate::auth::Access;
use crate::metrics::Metrics;

/// The body of every response: streamed, with I/O errors.
pub(crate) type Body = BoxBody<Bytes, std::io::Error>;

/// What every request handler shares.
#[derive(Debug)]
pub(crate) struct State {
    pub(crate) store: Store,
    pub(crate) access: Access,
    pub(crate) metrics: Metrics,
    /// The largest request body accepted, in bytes.
    #[allow(dead_code, reason = "read by the front-ends that accept uploads")]
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
    let response = route(&state, &request);
    state.metrics.observe(started.elapsed());
    Ok(response)
}

fn route<B>(state: &State, request: &Request<B>) -> Response<Body> {
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

/// A plain-text response.
pub(crate) fn text(status: StatusCode, body: impl Into<Bytes>) -> Response<Body> {
    let mut response = Response::new(
        Full::new(body.into())
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    response
}

/// A response with no body.
#[allow(dead_code, reason = "used by the front-ends")]
pub(crate) fn empty(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Empty::new().map_err(|never| match never {}).boxed());
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod test;
