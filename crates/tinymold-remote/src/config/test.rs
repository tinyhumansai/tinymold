//! Unit tests for reading the remote configuration.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use super::*;

fn env(vars: &[(&str, &str)]) -> Result<Option<RemoteConfig>> {
    let vars: HashMap<String, OsString> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    RemoteConfig::from_env(|name| vars.get(name).cloned())
}

#[test]
fn no_url_means_no_remote() {
    assert_eq!(env(&[]).unwrap(), None);
    assert_eq!(env(&[("TINYMOLD_REMOTE_URL", "  ")]).unwrap(), None);
}

#[test]
fn grpc_urls_become_channel_uris() {
    let uri = |url: &str| RemoteConfig::new(url).unwrap().endpoint;
    assert_eq!(uri("grpc://cache:9092"), "http://cache:9092");
    assert_eq!(
        uri("grpcs://cache.example.com"),
        "https://cache.example.com:443"
    );
    assert_eq!(
        uri("grpcs://cache.example.com:8443/"),
        "https://cache.example.com:8443"
    );
    assert_eq!(uri("http://127.0.0.1:9092"), "http://127.0.0.1:9092");
    assert!(RemoteConfig::new("grpcs://cache").unwrap().uses_tls());
    assert!(!RemoteConfig::new("grpc://cache:1").unwrap().uses_tls());
}

#[test]
fn malformed_urls_are_rejected() {
    for bad in [
        "cache:9092",
        "ftp://cache:21",
        "grpc://cache",
        "grpc://",
        "grpc://:9092",
        "grpc://user:pw@cache:9092",
        "grpc://cache:9092/path",
    ] {
        assert!(
            matches!(RemoteConfig::new(bad), Err(Error::InvalidUrl(url)) if url == bad),
            "{bad} accepted"
        );
    }
}

#[test]
fn every_variable_is_read() {
    let config = env(&[
        ("TINYMOLD_REMOTE_URL", "grpc://cache:9092"),
        ("TINYMOLD_REMOTE_AUTH", "ci:s3cret"),
        ("TINYMOLD_REMOTE_INSTANCE", "main"),
        ("TINYMOLD_REMOTE_TIMEOUT", "250ms"),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(config.authorization.as_deref(), Some("Basic Y2k6czNjcmV0"));
    assert_eq!(config.instance, "main");
    assert_eq!(config.timeout, Duration::from_millis(250));
    let defaults = env(&[("TINYMOLD_REMOTE_URL", "grpc://cache:9092")])
        .unwrap()
        .unwrap();
    assert_eq!(defaults.authorization, None);
    assert_eq!(defaults.timeout, DEFAULT_TIMEOUT);
}

#[test]
fn signing_and_trusted_keys_are_read() {
    let signer = crate::SigningKey::generate();
    let config = env(&[
        ("TINYMOLD_REMOTE_URL", "grpc://cache:9092"),
        ("TINYMOLD_REMOTE_SIGNING_KEY", &signer.to_hex()),
        ("TINYMOLD_REMOTE_TRUSTED_KEYS", &signer.public_hex()),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(config.signing_key, Some(signer.clone()));
    assert!(config.trusted_keys.is_some());
    assert!(!format!("{config:?}").contains(&signer.to_hex()));
    let defaults = env(&[("TINYMOLD_REMOTE_URL", "grpc://cache:9092")])
        .unwrap()
        .unwrap();
    assert_eq!(defaults.signing_key, None);
    assert_eq!(defaults.trusted_keys, None);
}

#[test]
fn malformed_keys_are_rejected() {
    let result = env(&[
        ("TINYMOLD_REMOTE_URL", "grpc://cache:9092"),
        ("TINYMOLD_REMOTE_SIGNING_KEY", "secret"),
    ]);
    assert!(
        matches!(result, Err(Error::InvalidSigningKey)),
        "{result:?}"
    );
    let result = env(&[
        ("TINYMOLD_REMOTE_URL", "grpc://cache:9092"),
        ("TINYMOLD_REMOTE_TRUSTED_KEYS", "abc"),
    ]);
    assert!(
        matches!(result, Err(Error::InvalidTrustedKey(_))),
        "{result:?}"
    );
}

#[test]
fn the_credential_is_never_printed() {
    let config = env(&[
        ("TINYMOLD_REMOTE_URL", "grpc://cache:9092"),
        ("TINYMOLD_REMOTE_AUTH", "ci:s3cret"),
    ])
    .unwrap()
    .unwrap();
    let debug = format!("{config:?}");
    assert!(
        !debug.contains("s3cret") && !debug.contains("Y2k6"),
        "{debug}"
    );
}

#[test]
fn malformed_credentials_and_timeouts_are_rejected() {
    for bad in ["nopassword", ":pw"] {
        let result = env(&[
            ("TINYMOLD_REMOTE_URL", "grpc://cache:9092"),
            ("TINYMOLD_REMOTE_AUTH", bad),
        ]);
        assert!(matches!(result, Err(Error::InvalidAuth)), "{bad} accepted");
    }
    for bad in ["soon", "0s", "1.5s", "-1"] {
        let result = env(&[
            ("TINYMOLD_REMOTE_URL", "grpc://cache:9092"),
            ("TINYMOLD_REMOTE_TIMEOUT", bad),
        ]);
        assert!(
            matches!(result, Err(Error::InvalidTimeout(text)) if text == bad),
            "{bad} accepted"
        );
    }
    let plain = env(&[
        ("TINYMOLD_REMOTE_URL", "grpc://cache:9092"),
        ("TINYMOLD_REMOTE_TIMEOUT", "7"),
    ]);
    assert_eq!(plain.unwrap().unwrap().timeout, Duration::from_secs(7));
}

#[test]
fn base64_matches_rfc_4648_vectors() {
    for (input, output) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64(input.as_bytes()), output);
    }
}
