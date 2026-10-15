//! Unit tests for the server error type.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn an_open_non_loopback_listener_names_the_address() {
    let addr: SocketAddr = "0.0.0.0:9090".parse().unwrap();

    assert_eq!(
        Error::OpenNonLoopback(addr).to_string(),
        "refusing to serve 0.0.0.0:9090 without a token file: only loopback addresses may be open"
    );
}

#[test]
fn token_file_errors_name_the_line() {
    let error = Error::TokenFile {
        line: 3,
        reason: "unknown permission",
    };

    assert_eq!(error.to_string(), "token file line 3: unknown permission");
}

#[test]
fn runtime_errors_keep_their_cause() {
    let error = Error::Runtime(std::io::Error::other("no threads"));

    assert!(std::error::Error::source(&error).is_some());
}
