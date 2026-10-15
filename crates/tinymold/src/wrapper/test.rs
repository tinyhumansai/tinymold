//! Unit tests for recognizing wrapper calls.

#![allow(clippy::unwrap_used)]

use super::*;

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn compilers_and_paths_are_wrapper_calls() {
    for program in [
        "rustc",
        "/home/u/.rustup/toolchains/stable/bin/rustc",
        "clippy-driver",
        "./tools/rustc-wrapper-shim",
        "rustc.exe",
    ] {
        let args = os(&[program, "--crate-name", "x"]);
        let (found, rest) = wrapper_call(&args).unwrap();
        assert_eq!(found, Path::new(program));
        assert_eq!(rest, &args[1..]);
    }
}

#[test]
fn subcommands_and_flags_are_not() {
    for first in [
        "hash", "cas", "doctor", "fresh", "snapshot", "--store", "-V", "help",
    ] {
        assert!(wrapper_call(&os(&[first, "x"])).is_none(), "{first}");
    }
    assert!(wrapper_call(&[]).is_none());
}

#[test]
fn the_wrapper_uses_the_default_store_and_an_optional_log() {
    let env = |name: &str| match name {
        "TINYMOLD_STORE" => Some("/store".into()),
        "TINYMOLD_LOG" => Some("/log".into()),
        "TINYMOLD_TRACE" => Some("/trace".into()),
        _ => None,
    };
    let config = wrapper_config(env).unwrap();
    assert_eq!(config.store, Path::new("/store"));
    assert_eq!(config.log.as_deref(), Some(Path::new("/log")));
    assert_eq!(config.trace.as_deref(), Some(Path::new("/trace")));

    let no_log = |name: &str| (name == "TINYMOLD_STORE").then(|| "/store".into());
    assert_eq!(wrapper_config(no_log).unwrap().log, None);
    assert_eq!(wrapper_config(no_log).unwrap().trace, None);
    assert!(wrapper_config(|_| None).is_none());
}

#[test]
fn the_executable_limit_comes_from_the_environment_and_ignores_malformed_sizes() {
    let with = |size: &'static str| {
        move |name: &str| match name {
            "TINYMOLD_STORE" => Some("/store".into()),
            "TINYMOLD_MAX_EXECUTABLE_SIZE" => Some(size.into()),
            _ => None,
        }
    };
    assert_eq!(
        wrapper_config(with("256M")).unwrap().max_executable,
        Some(256 << 20)
    );
    assert_eq!(
        wrapper_config(with("4096")).unwrap().max_executable,
        Some(4096)
    );
    assert_eq!(wrapper_config(with("lots")).unwrap().max_executable, None);
    let unset = |name: &str| (name == "TINYMOLD_STORE").then(|| "/store".into());
    assert_eq!(wrapper_config(unset).unwrap().max_executable, None);
}

#[test]
fn a_remote_is_configured_only_by_a_well_formed_url() {
    let none = |_: &str| None;
    assert!(wrapper_remote(none).is_none());
    let malformed = |name: &str| (name == "TINYMOLD_REMOTE_URL").then(|| OsString::from("nope"));
    assert!(wrapper_remote(malformed).is_none());
    let valid =
        |name: &str| (name == "TINYMOLD_REMOTE_URL").then(|| OsString::from("grpc://127.0.0.1:1"));
    assert_eq!(wrapper_remote(valid).is_some(), cfg!(feature = "remote"));
}
