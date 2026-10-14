//! Unit tests for dep-info parsing.

#![allow(clippy::unwrap_used)]

use super::*;

#[test]
fn reads_inputs_and_env_deps_as_rustc_writes_them() {
    let text = "/t/debug/deps/demo-1.d: src/lib.rs src/my\\ file.rs\n\
        \n\
        /t/debug/deps/libdemo-1.rlib: src/lib.rs src/my\\ file.rs\n\
        \n\
        src/lib.rs:\n\
        src/my\\ file.rs:\n\
        \n\
        # env-dep:CARGO_PKG_VERSION=0.1.0\n\
        # env-dep:MULTI=a\\nb\\\\c\\rd\n\
        # env-dep:UNSET\n\
        # checksum:ignored\n";
    let info = DepInfo::parse(text).unwrap();
    assert_eq!(info.inputs, ["src/lib.rs", "src/my file.rs"]);
    assert_eq!(
        info.env,
        [
            ("CARGO_PKG_VERSION".to_owned(), Some("0.1.0".to_owned())),
            ("MULTI".to_owned(), Some("a\nb\\c\rd".to_owned())),
            ("UNSET".to_owned(), None),
        ]
    );
}

#[test]
fn an_env_dep_without_a_name_is_malformed() {
    let error = DepInfo::parse("src/lib.rs:\n# env-dep:=x\n").unwrap_err();
    assert!(matches!(error, Error::MalformedDepInfo { line: 2 }));
}

#[test]
fn a_trailing_backslash_in_a_value_is_kept() {
    assert_eq!(unescape_env("a\\"), "a\\");
}
