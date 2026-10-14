//! Unit tests for the command-line error type.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn store_location_error_tells_the_user_how_to_fix_it() {
    assert_eq!(
        Error::NoStoreLocation.to_string(),
        "no store location: pass --store or set TINYMOLD_STORE"
    );
}

#[test]
fn library_errors_convert_and_render_transparently() {
    let hash = tinymold_hash::Error::InvalidHex { text: "x".into() };
    let expected = hash.to_string();
    assert_eq!(Error::from(hash).to_string(), expected);

    let cas = tinymold_cas::Error::DestinationExists("out".into());
    assert_eq!(Error::from(cas).to_string(), "destination out already exists");
}

#[test]
fn output_errors_keep_their_cause() {
    let error = Error::Output(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
    assert_eq!(error.to_string(), "failed to write output");
    assert!(std::error::Error::source(&error).is_some());
}
