//! Unit tests for the remote tier's error messages.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn configuration_errors_say_what_is_accepted() {
    assert_eq!(
        Error::InvalidUrl("x".into()).to_string(),
        "invalid remote URL \"x\": use grpc://host:port or grpcs://host[:port]"
    );
    assert_eq!(
        Error::InvalidAuth.to_string(),
        "invalid remote credential: use user:password"
    );
    assert_eq!(
        Error::InvalidTimeout("x".into()).to_string(),
        "invalid remote timeout \"x\": use whole seconds or milliseconds, such as 5s"
    );
}

#[test]
fn call_errors_name_the_call() {
    let status = Error::Status {
        call: "Read",
        status: Box::new(tonic::Status::not_found("blob")),
    };
    assert!(status.to_string().starts_with("the remote cache refused Read: "));
    assert!(std::error::Error::source(&status).is_some());
    assert_eq!(
        Error::Timeout("Write").to_string(),
        "the remote cache did not answer Write in time"
    );
    assert_eq!(
        Error::BlobMismatch {
            expected: "ab".into()
        }
        .to_string(),
        "the remote cache sent blob ab with the wrong content"
    );
    assert_eq!(
        Error::Malformed {
            key: "k".into(),
            reason: "it has no tinymold record"
        }
        .to_string(),
        "remote entry k is not a tinymold entry: it has no tinymold record"
    );
}

#[test]
fn setup_and_local_errors_render() {
    let runtime = Error::Runtime(std::io::Error::other("no threads"));
    assert_eq!(runtime.to_string(), "cannot start the remote client's runtime");
    let connect = Error::Connect(
        tonic::transport::Endpoint::from_shared("not a uri\n".to_owned()).unwrap_err(),
    );
    assert_eq!(connect.to_string(), "cannot connect to the remote cache");
    let cas = Error::from(tinymold_cas::Error::NotFound(tinymold_hash::Digest::of_bytes(b"")));
    assert!(cas.to_string().ends_with("is not in the store"));
    let wrap = Error::from(tinymold_wrap::Error::CorruptRecord {
        path: "r".into(),
        line: 1,
    });
    assert_eq!(wrap.to_string(), "cache record r is corrupt at line 1");
}
