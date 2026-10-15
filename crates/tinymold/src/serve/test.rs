//! Unit tests for `tinymold serve`'s argument handling.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use clap::Parser as _;

use super::*;
use crate::Command;

fn parse(args: &[&str]) -> (Cli, ServeArgs) {
    let cli = Cli::try_parse_from(args).unwrap();
    let Command::Serve(serve) = &cli.command else {
        panic!("not a serve command");
    };
    let serve = serve.clone();
    (cli, serve)
}

#[test]
fn arguments_become_a_server_configuration() {
    let (_, args) = parse(&[
        "tinymold",
        "serve",
        "--listen",
        "0.0.0.0:9092",
        "--tokens",
        "/etc/tokens",
        "--allow-anonymous-reads",
        "--tls-cert",
        "c.pem",
        "--tls-key",
        "k.pem",
        "--max-size",
        "2G",
        "--max-age",
        "30d",
        "--max-body",
        "16M",
        "--gc-interval",
        "5m",
    ]);
    let config = config(&args, PathBuf::from("/srv/cache"));

    assert_eq!(config.listen.to_string(), "0.0.0.0:9092");
    assert_eq!(config.store, PathBuf::from("/srv/cache"));
    assert_eq!(config.tokens, Some(PathBuf::from("/etc/tokens")));
    assert!(config.allow_anonymous_reads);
    assert_eq!(config.tls.unwrap().key, PathBuf::from("k.pem"));
    assert_eq!(config.max_size, Some(2 << 30));
    assert_eq!(config.max_age, Some(Duration::from_secs(30 * 86_400)));
    assert_eq!(config.max_body, 16 << 20);
    assert_eq!(config.gc_interval, Duration::from_secs(300));
}

#[test]
fn defaults_are_loopback_and_open() {
    let (_, args) = parse(&["tinymold", "serve"]);
    let config = config(&args, PathBuf::from("/srv/cache"));

    assert_eq!(config.listen.to_string(), tinymold_server::DEFAULT_LISTEN);
    assert_eq!(config.tokens, None);
    assert_eq!(config.max_body, tinymold_server::DEFAULT_MAX_BODY);
}

#[test]
fn a_tls_certificate_needs_a_key() {
    assert!(Cli::try_parse_from(["tinymold", "serve", "--tls-cert", "c.pem"]).is_err());
}

#[test]
fn serving_the_wrapper_store_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let path = store.to_str().unwrap();
    let (cli, args) = parse(&["tinymold", "--store", path, "serve", "--dir", path]);

    let error = run(&cli, &args, &mut Vec::new()).unwrap_err();

    assert!(matches!(error, Error::SharedStoreDir(_)), "{error:?}");
}

#[test]
fn a_bind_failure_is_a_server_error() {
    let dir = tempfile::tempdir().unwrap();
    let server_dir = dir.path().join("server");
    let wrapper = dir.path().join("wrapper");
    let (cli, args) = parse(&[
        "tinymold",
        "--store",
        wrapper.to_str().unwrap(),
        "serve",
        "--dir",
        server_dir.to_str().unwrap(),
        "--listen",
        "0.0.0.0:0",
    ]);

    let error = run(&cli, &args, &mut Vec::new()).unwrap_err();

    assert!(matches!(
        error,
        Error::Server(tinymold_server::Error::OpenNonLoopback(_))
    ));
}
