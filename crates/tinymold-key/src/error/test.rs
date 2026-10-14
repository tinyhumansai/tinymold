//! Unit tests for the cache-key error type.

use super::*;

#[test]
fn malformed_dep_info_names_the_line() {
    assert_eq!(
        Error::MalformedDepInfo { line: 7 }.to_string(),
        "malformed dep-info at line 7"
    );
}
