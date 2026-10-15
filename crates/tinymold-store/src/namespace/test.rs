//! Unit tests for namespaces and store keys.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn front_end_names_are_stable() {
    let names: Vec<_> = FrontEnd::ALL.iter().map(|front| front.as_str()).collect();

    assert_eq!(
        names,
        ["reapi", "bazel-http", "turbo", "nx", "gradle", "native"]
    );
}

#[test]
fn the_store_key_encoding_is_pinned() {
    let namespace = Namespace::new(FrontEnd::Turbo, "team_ci").unwrap();
    let key = namespace.key(b"7c6a9e3f0b1d2e4a").unwrap();

    assert_eq!(key.to_string(), PINNED_TURBO_KEY);
}

const PINNED_TURBO_KEY: &str = "PIN";

#[test]
fn the_same_client_key_in_two_front_ends_is_two_store_keys() {
    let turbo = Namespace::new(FrontEnd::Turbo, "ci").unwrap();
    let nx = Namespace::new(FrontEnd::Nx, "ci").unwrap();

    assert_ne!(turbo.key(b"abc").unwrap(), nx.key(b"abc").unwrap());
}

#[test]
fn the_same_client_key_for_two_tenants_is_two_store_keys() {
    let a = Namespace::new(FrontEnd::Turbo, "team-a").unwrap();
    let b = Namespace::new(FrontEnd::Turbo, "team-b").unwrap();

    assert_ne!(a.key(b"abc").unwrap(), b.key(b"abc").unwrap());
}

#[test]
fn tenant_and_key_boundaries_cannot_bleed_into_each_other() {
    let a = Namespace::new(FrontEnd::Gradle, "ab").unwrap();
    let b = Namespace::new(FrontEnd::Gradle, "a").unwrap();

    assert_ne!(a.key(b"c").unwrap(), b.key(b"bc").unwrap());
}

#[test]
fn an_overlong_tenant_is_refused() {
    let error = Namespace::new(FrontEnd::Nx, "t".repeat(MAX_TENANT + 1)).unwrap_err();

    assert!(matches!(error, Error::TenantTooLong { len } if len == MAX_TENANT + 1));
}

#[test]
fn an_overlong_client_key_is_refused() {
    let namespace = Namespace::new(FrontEnd::Reapi, "").unwrap();
    let error = namespace.key(&[0; MAX_CLIENT_KEY + 1]).unwrap_err();

    assert!(matches!(error, Error::KeyTooLong { len } if len == MAX_CLIENT_KEY + 1));
}

#[test]
fn a_namespace_reports_what_it_was_built_from() {
    let namespace = Namespace::new(FrontEnd::BazelHttp, "ccache").unwrap();

    assert_eq!(namespace.front_end(), FrontEnd::BazelHttp);
    assert_eq!(namespace.tenant(), "ccache");
    assert_eq!(namespace.to_string(), "bazel-http/ccache");
}
