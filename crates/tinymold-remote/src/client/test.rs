//! Unit tests for the REAPI calls, against the in-process fake server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::config::DEFAULT_TIMEOUT;
use crate::fake::Server;
use crate::proto::OutputFile;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// A client for `server`, created inside `runtime`.
fn client(runtime: &tokio::runtime::Runtime, config: &RemoteConfig) -> Client {
    let _entered = runtime.enter();
    Client::new(config).unwrap()
}

fn config(server: &Server) -> RemoteConfig {
    RemoteConfig::new(&server.url()).unwrap()
}

#[test]
fn capabilities_report_sha256_and_whether_updates_are_allowed() {
    let server = Server::start();
    let runtime = runtime();
    let client = client(&runtime, &config(&server));
    let capabilities = runtime.block_on(client.capabilities()).unwrap();
    let cache = capabilities.cache_capabilities.unwrap();
    assert_eq!(cache.digest_functions, [DIGEST_SHA256]);
    assert!(
        cache
            .action_cache_update_capabilities
            .unwrap()
            .update_enabled
    );
}

#[test]
fn an_action_result_round_trips_and_a_missing_one_is_none() {
    let server = Server::start();
    let runtime = runtime();
    let client = client(&runtime, &config(&server));
    let blob = naming::blob(b"bytes");
    let key = naming::blob(b"key");
    let result = ActionResult {
        output_files: vec![OutputFile {
            path: "out".to_owned(),
            digest: Some(blob.clone()),
            is_executable: true,
        }],
        exit_code: 0,
    };

    runtime.block_on(async {
        assert_eq!(client.get_action_result(key.clone()).await.unwrap(), None);
        assert_eq!(
            client.find_missing(vec![blob.clone()]).await.unwrap(),
            std::slice::from_ref(&blob)
        );
        client.write(&blob, b"bytes").await.unwrap();
        assert_eq!(client.find_missing(vec![blob.clone()]).await.unwrap(), []);
        client
            .update_action_result(key.clone(), result.clone())
            .await
            .unwrap();
        assert_eq!(client.get_action_result(key).await.unwrap(), Some(result));
    });
}

#[test]
fn large_and_empty_blobs_survive_chunked_transfers() {
    let server = Server::start();
    let runtime = runtime();
    let client = client(&runtime, &config(&server));
    let large: Vec<u8> = (0..(CHUNK * 2 + 17))
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    for data in [large, Vec::new()] {
        let digest = naming::blob(&data);
        runtime.block_on(async {
            client.write(&digest, &data).await.unwrap();
            assert_eq!(client.read(&digest).await.unwrap(), data);
        });
    }
}

#[test]
fn a_blob_whose_content_does_not_match_is_rejected() {
    let server = Server::start();
    let runtime = runtime();
    let client = client(&runtime, &config(&server));
    let digest = naming::blob(b"real");
    runtime.block_on(client.write(&digest, b"real")).unwrap();
    server.with(|state| state.corrupt.insert(digest.hash.clone()));

    let result = runtime.block_on(client.read(&digest));
    assert!(matches!(result, Err(Error::BlobMismatch { expected }) if expected == digest.hash));
}

#[test]
fn reading_a_missing_blob_is_a_not_found_status() {
    let server = Server::start();
    let runtime = runtime();
    let client = client(&runtime, &config(&server));
    let result = runtime.block_on(client.read(&naming::blob(b"absent")));
    assert!(
        matches!(&result, Err(Error::Status { call: "Read", status }) if status.code() == Code::NotFound),
        "{result:?}"
    );
}

#[test]
fn the_credential_is_sent_on_every_call() {
    let server = Server::start();
    server.with(|state| state.required_auth = Some("Basic Y2k6czNjcmV0".to_owned()));
    let runtime = runtime();

    let anonymous = client(&runtime, &config(&server));
    let refused = runtime.block_on(anonymous.capabilities());
    assert!(
        matches!(&refused, Err(Error::Status { status, .. }) if status.code() == Code::Unauthenticated),
        "{refused:?}"
    );

    let mut with_auth = config(&server);
    with_auth.authorization = Some("Basic Y2k6czNjcmV0".to_owned());
    let client = client(&runtime, &with_auth);
    runtime.block_on(async {
        client.capabilities().await.unwrap();
        let digest = naming::blob(b"x");
        client.write(&digest, b"x").await.unwrap();
        assert_eq!(client.read(&digest).await.unwrap(), b"x");
    });
}

#[test]
fn a_read_only_server_refuses_writes() {
    let server = Server::start();
    server.with(|state| state.read_only = true);
    let runtime = runtime();
    let client = client(&runtime, &config(&server));
    let digest = naming::blob(b"x");
    let write = runtime.block_on(client.write(&digest, b"x"));
    assert!(
        matches!(&write, Err(Error::Status { call: "Write", status }) if status.code() == Code::PermissionDenied),
        "{write:?}"
    );
    let update = runtime.block_on(client.update_action_result(digest, ActionResult::default()));
    assert!(
        matches!(
            &update,
            Err(Error::Status {
                call: "UpdateActionResult",
                ..
            })
        ),
        "{update:?}"
    );
}

#[test]
fn a_server_that_stalls_times_out() {
    let server = Server::start();
    server.with(|state| state.delay = Some(Duration::from_secs(5)));
    let runtime = runtime();
    let mut quick = config(&server);
    quick.timeout = Duration::from_millis(100);
    let client = client(&runtime, &quick);
    let result = runtime.block_on(client.capabilities());
    assert!(
        matches!(result, Err(Error::Timeout("GetCapabilities"))),
        "{result:?}"
    );
    let read = runtime.block_on(client.read(&naming::blob(b"x")));
    assert!(matches!(read, Err(Error::Timeout("Read"))), "{read:?}");
}

#[test]
fn an_unreachable_server_fails_the_call() {
    // Bind and drop a listener so the port is known to be closed.
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let runtime = runtime();
    let client = client(
        &runtime,
        &RemoteConfig::new(&format!("grpc://{address}")).unwrap(),
    );
    let result = runtime.block_on(client.get_action_result(naming::blob(b"k")));
    assert!(matches!(result, Err(Error::Status { .. })), "{result:?}");
}

#[test]
fn tls_endpoints_and_credentials_are_set_up() {
    let runtime = runtime();
    let mut config = RemoteConfig::new("grpcs://cache.example.com").unwrap();
    config.authorization = Some("Basic eDp5".to_owned());
    client(&runtime, &config);

    config.authorization = Some("not\na header".to_owned());
    let _entered = runtime.enter();
    assert!(matches!(Client::new(&config), Err(Error::InvalidAuth)));
}

#[test]
fn transfers_get_time_in_proportion_to_their_size() {
    let runtime = runtime();
    let client = client(&runtime, &RemoteConfig::new("grpc://cache:1").unwrap());
    let small = naming::blob(b"x");
    let large = Digest {
        hash: small.hash.clone(),
        size_bytes: 40 << 20,
    };
    assert_eq!(client.allowance(&small, 0), DEFAULT_TIMEOUT);
    assert_eq!(
        client.allowance(&large, 0),
        DEFAULT_TIMEOUT + Duration::from_secs(10)
    );
}

#[test]
fn a_transfer_sharing_the_link_gets_time_for_everything_in_flight() {
    // One 1-byte blob among 40 MiB of others moves at their pace, not its own.
    let runtime = runtime();
    let client = client(&runtime, &RemoteConfig::new("grpc://cache:1").unwrap());
    let small = naming::blob(b"x");
    assert_eq!(
        client.allowance(&small, 40 << 20),
        DEFAULT_TIMEOUT + Duration::from_secs(10)
    );
    let large = Digest {
        hash: small.hash.clone(),
        size_bytes: 80 << 20,
    };
    assert_eq!(
        client.allowance(&large, 40 << 20),
        DEFAULT_TIMEOUT + Duration::from_secs(20)
    );
}

#[test]
fn a_key_sent_without_a_size_is_refused_as_bazel_remote_refuses_it() {
    let server = Server::start();
    let runtime = runtime();
    let client = client(&runtime, &config(&server));
    let unsized_key = Digest {
        hash: naming::blob(b"key").hash,
        size_bytes: 0,
    };
    let result = runtime.block_on(client.get_action_result(unsized_key));
    assert!(
        matches!(&result, Err(Error::Status { status, .. }) if status.code() == Code::InvalidArgument),
        "{result:?}"
    );
    let sized = naming::action_key(&tinymold_hash::Digest::of_bytes(b"key"));
    assert_eq!(
        runtime.block_on(client.get_action_result(sized)).unwrap(),
        None
    );
}

#[test]
fn a_blob_declared_too_large_or_negative_is_refused_before_reading() {
    let server = Server::start();
    let runtime = runtime();
    let client = client(&runtime, &config(&server));
    for size_bytes in [i64::MAX, -1] {
        let poisoned = Digest {
            hash: naming::blob(b"real").hash,
            size_bytes,
        };
        let result = runtime.block_on(client.read(&poisoned));
        assert!(
            matches!(&result, Err(Error::BlobSize { size, .. }) if *size == size_bytes),
            "{result:?}"
        );
        assert_eq!(client.allowance(&poisoned, 0), DEFAULT_TIMEOUT);
    }
}

#[test]
fn a_stream_longer_than_its_declared_size_is_cut_off() {
    let server = Server::start();
    let runtime = runtime();
    let client = client(&runtime, &config(&server));
    let digest = naming::blob(b"twelve bytes");
    runtime
        .block_on(client.write(&digest, b"twelve bytes"))
        .unwrap();
    let understated = Digest {
        hash: digest.hash.clone(),
        size_bytes: 4,
    };
    let result = runtime.block_on(client.read(&understated));
    assert!(
        matches!(&result, Err(Error::BlobSize { size: 4, .. })),
        "{result:?}"
    );
}

#[test]
fn the_credential_is_kept_out_of_debug_output() {
    let runtime = runtime();
    let mut config = RemoteConfig::new("grpc://cache:1").unwrap();
    config.authorization = Some("Basic c2VjcmV0".to_owned());
    let client = client(&runtime, &config);
    assert!(!format!("{client:?}").contains("c2VjcmV0"));
}
