//! Protocol front-ends: each maps one wire protocol onto the store.
//!
//! A front-end parses its protocol's requests into a namespace, a key and
//! an operation, checks access, calls the store on a blocking thread, and
//! answers in its protocol's terms. None of them keeps state of its own.

pub(crate) mod bazel_http;

use http::{HeaderValue, Response, StatusCode, header};

use crate::auth::Denied;
use crate::route::{self, Body};

/// The answer to a refused request: `401` with a basic-auth challenge, so
/// clients that wait for one (Gradle, curl) send their credential, or `403`.
pub(crate) fn denied(denial: Denied) -> Response<Body> {
    match denial {
        Denied::Unauthenticated => {
            let mut response = route::text(StatusCode::UNAUTHORIZED, "unauthorized\n");
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"tinymold\""),
            );
            response
        }
        Denied::Forbidden => route::text(StatusCode::FORBIDDEN, "forbidden\n"),
    }
}

/// Runs a store call on a blocking thread. A panicked call is a server
/// error, answered as one, never a crash.
pub(crate) async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    tokio::task::spawn_blocking(call).await.ok()
}

/// The `Content-Length` a request declares, if any.
pub(crate) fn declared_length<B>(request: &http::Request<B>) -> Option<u64> {
    request
        .headers()
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}
