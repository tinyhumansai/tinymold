//! Unit tests for token files and access checks.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

const FILE: &str = "\
# comment
writer   *:write

reader   turbo/team-a:read nx:read
";

fn headers(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(http::header::AUTHORIZATION, value.parse().unwrap());
    headers
}

fn basic(user: &str, password: &str) -> HeaderMap {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    headers(&format!("Basic {encoded}"))
}

fn ns(front_end: FrontEnd, tenant: &str) -> Namespace {
    Namespace::new(front_end, tenant).unwrap()
}

fn access(anonymous_reads: bool) -> Access {
    Access::new(Tokens::parse(FILE).unwrap(), anonymous_reads)
}

#[test]
fn a_write_token_may_read_and_write_anywhere() {
    let access = access(false);
    let writer = headers("Bearer writer");

    for front_end in FrontEnd::ALL {
        let namespace = ns(front_end, "anything");
        assert_eq!(access.check(&writer, &namespace, Permission::Write), Ok(()));
        assert_eq!(access.check(&writer, &namespace, Permission::Read), Ok(()));
    }
}

#[test]
fn a_read_token_is_limited_to_its_scopes() {
    let access = access(false);
    let reader = headers("Bearer reader");

    assert_eq!(
        access.check(&reader, &ns(FrontEnd::Turbo, "team-a"), Permission::Read),
        Ok(())
    );
    assert_eq!(
        access.check(&reader, &ns(FrontEnd::Nx, "any"), Permission::Read),
        Ok(())
    );
    assert_eq!(
        access.check(&reader, &ns(FrontEnd::Turbo, "team-b"), Permission::Read),
        Err(Denied::Forbidden)
    );
    assert_eq!(
        access.check(&reader, &ns(FrontEnd::Turbo, "team-a"), Permission::Write),
        Err(Denied::Forbidden)
    );
}

#[test]
fn basic_auth_presents_the_password_as_the_token() {
    let access = access(false);

    assert_eq!(
        access.check(&basic("gradle", "writer"), &ns(FrontEnd::Gradle, ""), Permission::Write),
        Ok(())
    );
    assert_eq!(
        access.check(&basic("writer", "nope"), &ns(FrontEnd::Gradle, ""), Permission::Read),
        Err(Denied::Unauthenticated)
    );
}

#[test]
fn a_missing_or_unknown_credential_is_unauthenticated() {
    let access = access(false);
    let namespace = ns(FrontEnd::Reapi, "");

    assert_eq!(
        access.check(&HeaderMap::new(), &namespace, Permission::Read),
        Err(Denied::Unauthenticated)
    );
    assert_eq!(
        access.check(&headers("Bearer guess"), &namespace, Permission::Read),
        Err(Denied::Unauthenticated)
    );
    assert_eq!(
        access.check(&headers("Digest writer"), &namespace, Permission::Read),
        Err(Denied::Unauthenticated)
    );
}

#[test]
fn anonymous_reads_allow_reading_but_never_writing() {
    let access = access(true);
    let namespace = ns(FrontEnd::Turbo, "team-b");

    assert_eq!(access.check(&HeaderMap::new(), &namespace, Permission::Read), Ok(()));
    assert_eq!(
        access.check(&HeaderMap::new(), &namespace, Permission::Write),
        Err(Denied::Unauthenticated)
    );
    assert_eq!(
        access.check(&headers("Bearer reader"), &namespace, Permission::Read),
        Ok(())
    );
}

#[test]
fn an_open_policy_allows_everything() {
    let access = Access::open();

    assert!(access.is_open());
    assert_eq!(
        access.check(&HeaderMap::new(), &ns(FrontEnd::Nx, ""), Permission::Write),
        Ok(())
    );
}

#[test]
fn malformed_lines_are_reported_by_number() {
    for (text, reason) in [
        ("token", "a token needs at least one scope"),
        ("token turbo", "a scope ends in :read or :write"),
        ("token turbo:own", "unknown permission"),
        ("token cargo:read", "unknown front-end"),
    ] {
        let error = Tokens::parse(&format!("# header\n{text}\n")).unwrap_err();
        assert!(
            matches!(error, Error::TokenFile { line: 2, reason: got } if got == reason),
            "{text}: {error:?}"
        );
    }
}

#[test]
fn a_debug_dump_never_shows_a_token() {
    let tokens = Tokens::parse(FILE).unwrap();

    assert!(!format!("{tokens:?}").contains("writer"));
}

#[test]
fn a_missing_token_file_is_a_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let error = Tokens::load(&dir.path().join("absent")).unwrap_err();

    assert!(matches!(error, Error::ReadFile { .. }));
}
