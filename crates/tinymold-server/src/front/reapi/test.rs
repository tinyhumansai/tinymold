//! Unit tests for the REAPI front-end: a real server on a loopback port,
//! called through tonic's generic client.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use http::uri::PathAndQuery;
use tonic::transport::Channel;

use super::types::{Directory, FileNode, OutputDirectoryRef, OutputFileRef, UpdateBlob};
use super::*;
use crate::{Config, Server};

const CAPABILITIES: &str = "/build.bazel.remote.execution.v2.Capabilities/GetCapabilities";
const GET_RESULT: &str = "/build.bazel.remote.execution.v2.ActionCache/GetActionResult";
const UPDATE_RESULT: &str = "/build.bazel.remote.execution.v2.ActionCache/UpdateActionResult";
const FIND_MISSING: &str =
    "/build.bazel.remote.execution.v2.ContentAddressableStorage/FindMissingBlobs";
const BATCH_UPDATE: &str =
    "/build.bazel.remote.execution.v2.ContentAddressableStorage/BatchUpdateBlobs";
const BATCH_READ: &str =
    "/build.bazel.remote.execution.v2.ContentAddressableStorage/BatchReadBlobs";
const READ: &str = "/google.bytestream.ByteStream/Read";
const WRITE: &str = "/google.bytestream.ByteStream/Write";

/// An `ActionResult` with fields the server does not declare, to check
/// they come back unchanged.
#[derive(Clone, PartialEq, prost::Message)]
struct FullResult {
    #[prost(message, repeated, tag = "2")]
    output_files: Vec<OutputFileRef>,
    #[prost(message, repeated, tag = "3")]
    output_directories: Vec<OutputDirectoryRef>,
    #[prost(int32, tag = "4")]
    exit_code: i32,
    #[prost(bytes = "vec", tag = "5")]
    stdout_raw: Vec<u8>,
    #[prost(bytes = "vec", tag = "9")]
    execution_metadata: Vec<u8>,
}

struct Running {
    channel: Channel,
    store: Store,
    addr: std::net::SocketAddr,
    _stop: tokio::sync::oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

async fn start_with(tokens: Option<&str>) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::new(dir.path().join("store"));
    config.listen = "127.0.0.1:0".parse().unwrap();
    config.max_body = 8 << 20;
    if let Some(tokens) = tokens {
        let path = dir.path().join("tokens");
        std::fs::write(&path, tokens).unwrap();
        config.tokens = Some(path);
    }
    let server = Server::bind(&config).unwrap();
    let addr = server.local_addr();
    let store = Store::open(&config.store).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    Running {
        channel,
        store,
        addr,
        _stop: stop,
        _dir: dir,
    }
}

async fn start() -> Running {
    start_with(None).await
}

fn with_auth<T>(message: T, auth: Option<&str>) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    if let Some(auth) = auth {
        request
            .metadata_mut()
            .insert("authorization", auth.parse().unwrap());
    }
    request
}

async fn unary<Req, Res>(
    server: &Running,
    path: &'static str,
    message: Req,
    auth: Option<&str>,
) -> Result<Res, Status>
where
    Req: prost::Message + Send + Sync + 'static,
    Res: prost::Message + Default + Send + Sync + 'static,
{
    let mut grpc = tonic::client::Grpc::new(server.channel.clone());
    grpc.ready().await.unwrap();
    grpc.unary(
        with_auth(message, auth),
        PathAndQuery::from_static(path),
        ProstCodec::<Req, Res>::default(),
    )
    .await
    .map(tonic::Response::into_inner)
}

fn len(bytes: &[u8]) -> i64 {
    i64::try_from(bytes.len()).unwrap()
}

fn digest(bytes: &[u8]) -> Digest {
    Digest {
        hash: Sha256::of_bytes(bytes).to_hex(),
        size_bytes: len(bytes),
    }
}

async fn write_blob(
    server: &Running,
    instance: &str,
    bytes: &[u8],
    auth: Option<&str>,
) -> Result<i64, Status> {
    let digest = digest(bytes);
    let resource = format!(
        "{instance}{}uploads/0f5d/blobs/{}/{}",
        if instance.is_empty() { "" } else { "/" },
        digest.hash,
        digest.size_bytes
    );
    // Several chunks, so the server has to put a stream together.
    let mut messages = Vec::new();
    let mut offset = 0;
    for chunk in bytes
        .chunks(100_000)
        .chain(bytes.is_empty().then_some(&[][..]))
    {
        messages.push(WriteRequest {
            resource_name: if offset == 0 {
                resource.clone()
            } else {
                String::new()
            },
            write_offset: offset,
            finish_write: offset + len(chunk) == len(bytes),
            data: Bytes::copy_from_slice(chunk),
        });
        offset += len(chunk);
    }
    let mut grpc = tonic::client::Grpc::new(server.channel.clone());
    grpc.ready().await.unwrap();
    grpc.client_streaming(
        with_auth(tokio_stream::iter(messages), auth),
        PathAndQuery::from_static(WRITE),
        ProstCodec::<WriteRequest, WriteResponse>::default(),
    )
    .await
    .map(|response| response.into_inner().committed_size)
}

async fn read_blob(server: &Running, resource: String) -> Result<Vec<u8>, Status> {
    read_range(server, resource, 0, 0).await
}

async fn read_range(
    server: &Running,
    resource: String,
    offset: i64,
    limit: i64,
) -> Result<Vec<u8>, Status> {
    let mut grpc = tonic::client::Grpc::new(server.channel.clone());
    grpc.ready().await.unwrap();
    let mut stream = grpc
        .server_streaming(
            tonic::Request::new(ReadRequest {
                resource_name: resource,
                read_offset: offset,
                read_limit: limit,
            }),
            PathAndQuery::from_static(READ),
            ProstCodec::<ReadRequest, ReadResponse>::default(),
        )
        .await?
        .into_inner();
    let mut data = Vec::new();
    while let Some(chunk) = stream.message().await? {
        data.extend_from_slice(&chunk.data);
    }
    Ok(data)
}

fn update(instance: &str, action: &[u8], result: &FullResult) -> UpdateActionResultRequest {
    UpdateActionResultRequest {
        instance_name: instance.to_owned(),
        action_digest: Some(digest(action)),
        action_result: result.encode_to_vec(),
        digest_function: DIGEST_SHA256,
    }
}

fn get(instance: &str, action: &[u8]) -> GetActionResultRequest {
    GetActionResultRequest {
        instance_name: instance.to_owned(),
        action_digest: Some(digest(action)),
        digest_function: DIGEST_SHA256,
    }
}

fn result_with_file(bytes: &[u8]) -> FullResult {
    FullResult {
        output_files: vec![OutputFileRef {
            digest: Some(digest(bytes)),
        }],
        output_directories: Vec::new(),
        exit_code: 0,
        stdout_raw: b"compiled".to_vec(),
        execution_metadata: b"\x0a\x04host".to_vec(),
    }
}

#[test]
fn the_message_encodings_are_pinned() {
    let digest = Digest {
        hash: "ab".into(),
        size_bytes: 3,
    };
    assert_eq!(digest.encode_to_vec(), b"\x0a\x02ab\x10\x03");

    let request = UpdateActionResultRequest {
        instance_name: "i".into(),
        action_digest: None,
        action_result: b"\x20\x01".to_vec(),
        digest_function: DIGEST_SHA256,
    };
    assert_eq!(
        request.encode_to_vec(),
        b"\x0a\x01i\x1a\x02\x20\x01\x28\x01"
    );
    assert_eq!(Raw(b"\x20\x01".to_vec()).encode_to_vec(), b"\x20\x01");

    let write = WriteRequest {
        resource_name: "r".into(),
        write_offset: 1,
        finish_write: true,
        data: Bytes::from_static(b"d"),
    };
    assert_eq!(write.encode_to_vec(), b"\x0a\x01r\x10\x01\x18\x01\x52\x01d");
}

#[test]
fn resource_names_split_into_instance_and_digest() {
    let (instance, digest) = read_resource("team/main/blobs/abc/12").unwrap();
    assert_eq!(
        (instance, digest.hash.as_str(), digest.size_bytes),
        ("team/main", "abc", 12)
    );

    let (instance, digest) = read_resource("blobs/abc/12").unwrap();
    assert_eq!((instance, digest.size_bytes), ("", 12));

    let (instance, digest) = write_resource("ci/uploads/uuid/blobs/abc/7/extra").unwrap();
    assert_eq!((instance, digest.hash.as_str()), ("ci", "abc"));

    assert!(read_resource("compressed-blobs/zstd/abc/12").is_err());
    assert!(write_resource("uploads/uuid/compressed-blobs/zstd/abc/12").is_err());
    assert!(read_resource("blobs/abc/twelve").is_err());
    assert!(read_resource("blobs/abc").is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_advertise_sha256_and_api_versions() {
    let server = start().await;
    let capabilities: ServerCapabilities = unary(
        &server,
        CAPABILITIES,
        GetCapabilitiesRequest::default(),
        None,
    )
    .await
    .unwrap();

    let cache = capabilities.cache_capabilities.unwrap();
    assert_eq!(cache.digest_functions, [DIGEST_SHA256]);
    assert!(
        cache
            .action_cache_update_capabilities
            .unwrap()
            .update_enabled
    );
    assert_eq!(cache.max_batch_total_size_bytes, MAX_BATCH);
    assert_eq!(capabilities.high_api_version.unwrap().major, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_action_result_needs_its_blobs_and_comes_back_byte_for_byte() {
    let server = start().await;
    let result = result_with_file(b"object file");

    let refused = unary::<_, Raw>(&server, UPDATE_RESULT, update("", b"action", &result), None)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::FailedPrecondition);

    write_blob(&server, "", b"object file", None).await.unwrap();
    unary::<_, Raw>(&server, UPDATE_RESULT, update("", b"action", &result), None)
        .await
        .unwrap();
    let fetched: FullResult = unary(&server, GET_RESULT, get("", b"action"), None)
        .await
        .unwrap();

    assert_eq!(fetched, result);
}

#[tokio::test(flavor = "multi_thread")]
async fn action_results_are_scoped_to_their_instance() {
    let server = start().await;
    write_blob(&server, "a", b"out", None).await.unwrap();
    unary::<_, Raw>(
        &server,
        UPDATE_RESULT,
        update("a", b"act", &result_with_file(b"out")),
        None,
    )
    .await
    .unwrap();

    let other = unary::<_, FullResult>(&server, GET_RESULT, get("b", b"act"), None).await;

    assert_eq!(other.unwrap_err().code(), tonic::Code::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_result_whose_blobs_were_collected_is_not_found() {
    let server = start().await;
    write_blob(&server, "", b"out", None).await.unwrap();
    unary::<_, Raw>(
        &server,
        UPDATE_RESULT,
        update("", b"act", &result_with_file(b"out")),
        None,
    )
    .await
    .unwrap();
    let blob = server
        .store
        .find_sha256(&Sha256::of_bytes(b"out"))
        .unwrap()
        .unwrap();
    server.store.cas().remove(&blob).unwrap();

    let gone = unary::<_, FullResult>(&server, GET_RESULT, get("", b"act"), None).await;

    assert_eq!(gone.unwrap_err().code(), tonic::Code::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_output_directory_needs_every_file_of_its_tree() {
    let server = start().await;
    let tree = Tree {
        root: Some(Directory {
            files: vec![FileNode {
                digest: Some(digest(b"inside the tree")),
            }],
        }),
        children: Vec::new(),
    }
    .encode_to_vec();
    write_blob(&server, "", &tree, None).await.unwrap();
    let result = FullResult {
        output_directories: vec![OutputDirectoryRef {
            tree_digest: Some(digest(&tree)),
        }],
        ..FullResult::default()
    };

    let refused = unary::<_, Raw>(&server, UPDATE_RESULT, update("", b"dir", &result), None).await;
    assert_eq!(refused.unwrap_err().code(), tonic::Code::FailedPrecondition);

    write_blob(&server, "", b"inside the tree", None)
        .await
        .unwrap();
    unary::<_, Raw>(&server, UPDATE_RESULT, update("", b"dir", &result), None)
        .await
        .unwrap();
    let key = Namespace::new(FrontEnd::Reapi, "")
        .unwrap()
        .key(Sha256::of_bytes(b"dir").to_hex().as_bytes())
        .unwrap();
    // The tree and the file inside it are both kept by the entry.
    assert_eq!(server.store.get(&key).unwrap().unwrap().blobs.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn find_missing_blobs_sees_every_protocol_and_the_empty_blob() {
    let server = start().await;
    // Uploaded through Bazel's HTTP protocol, not REAPI.
    let client = reqwest_put(
        server.addr,
        &format!("/cas/{}", Sha256::of_bytes(b"via http")),
        b"via http",
    )
    .await;
    assert_eq!(client, 200);

    let response: FindMissingBlobsResponse = unary(
        &server,
        FIND_MISSING,
        FindMissingBlobsRequest {
            instance_name: String::new(),
            blob_digests: vec![digest(b"via http"), digest(b""), digest(b"absent")],
            digest_function: DIGEST_UNKNOWN,
        },
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.missing_blob_digests, [digest(b"absent")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_updates_report_each_blob() {
    let server = start().await;
    let mut wrong = digest(b"claimed");
    wrong.size_bytes = 3;
    let response: BatchUpdateBlobsResponse = unary(
        &server,
        BATCH_UPDATE,
        BatchUpdateBlobsRequest {
            instance_name: String::new(),
            requests: vec![
                UpdateBlob {
                    digest: Some(digest(b"good")),
                    data: b"good".to_vec(),
                    compressor: 0,
                },
                UpdateBlob {
                    digest: Some(wrong),
                    data: b"bad".to_vec(),
                    compressor: 0,
                },
                UpdateBlob {
                    digest: Some(digest(b"zstd")),
                    data: b"zstd".to_vec(),
                    compressor: 1,
                },
            ],
            digest_function: DIGEST_SHA256,
        },
        None,
    )
    .await
    .unwrap();

    let codes: Vec<i32> = response
        .responses
        .iter()
        .map(|blob| blob.status.as_ref().unwrap().code)
        .collect();
    assert_eq!(
        codes,
        [
            0,
            tonic::Code::InvalidArgument as i32,
            tonic::Code::InvalidArgument as i32
        ]
    );
    assert!(
        server
            .store
            .find_sha256(&Sha256::of_bytes(b"good"))
            .unwrap()
            .is_some()
    );
    assert!(
        server
            .store
            .find_sha256(&Sha256::of_bytes(b"bad"))
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_reads_return_present_blobs_and_not_found_for_the_rest() {
    let server = start().await;
    write_blob(&server, "", b"present", None).await.unwrap();
    let response: BatchReadBlobsResponse = unary(
        &server,
        BATCH_READ,
        BatchReadBlobsRequest {
            instance_name: String::new(),
            digests: vec![digest(b"present"), digest(b"absent")],
            digest_function: DIGEST_SHA256,
        },
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.responses[0].data, b"present");
    assert_eq!(response.responses[0].status.as_ref().unwrap().code, 0);
    assert_eq!(
        response.responses[1].status.as_ref().unwrap().code,
        tonic::Code::NotFound as i32
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bytestream_round_trips_a_large_blob_and_honours_ranges() {
    let server = start().await;
    let blob: Vec<u8> = (0..700_000_u32)
        .map(|i| u8::try_from(i % 241).unwrap())
        .collect();
    let digest = digest(&blob);

    assert_eq!(
        write_blob(&server, "ci", &blob, None).await.unwrap(),
        digest.size_bytes
    );
    let resource = format!("ci/blobs/{}/{}", digest.hash, digest.size_bytes);

    let read = read_blob(&server, resource.clone()).await.unwrap();
    assert!(read == blob, "the blob read back differs");
    assert_eq!(
        read_range(&server, resource.clone(), 10, 5).await.unwrap(),
        &blob[10..15]
    );
    assert_eq!(
        read_range(&server, resource, 800_000, 0)
            .await
            .unwrap_err()
            .code(),
        tonic::Code::OutOfRange
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bytestream_reads_of_absent_blobs_are_not_found() {
    let server = start().await;
    let digest = digest(b"absent");

    let error = read_blob(
        &server,
        format!("blobs/{}/{}", digest.hash, digest.size_bytes),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), tonic::Code::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_whose_data_does_not_match_is_refused_and_not_stored() {
    let server = start().await;
    let digest = digest(b"claimed!");
    let mut grpc = tonic::client::Grpc::new(server.channel.clone());
    grpc.ready().await.unwrap();
    let messages = vec![WriteRequest {
        resource_name: format!("uploads/u/blobs/{}/{}", digest.hash, digest.size_bytes),
        write_offset: 0,
        finish_write: true,
        data: Bytes::from_static(b"actually"),
    }];

    let error = grpc
        .client_streaming(
            tonic::Request::new(tokio_stream::iter(messages)),
            PathAndQuery::from_static(WRITE),
            ProstCodec::<WriteRequest, WriteResponse>::default(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(
        server
            .store
            .find_sha256(&Sha256::of_bytes(b"actually"))
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn non_contiguous_writes_are_refused() {
    let server = start().await;
    let digest = digest(b"abcdef");
    let resource = format!("uploads/u/blobs/{}/{}", digest.hash, digest.size_bytes);
    let messages = vec![
        WriteRequest {
            resource_name: resource,
            write_offset: 0,
            finish_write: false,
            data: Bytes::from_static(b"abc"),
        },
        WriteRequest {
            resource_name: String::new(),
            write_offset: 5,
            finish_write: true,
            data: Bytes::from_static(b"def"),
        },
    ];
    let mut grpc = tonic::client::Grpc::new(server.channel.clone());
    grpc.ready().await.unwrap();

    let error = grpc
        .client_streaming(
            tonic::Request::new(tokio_stream::iter(messages)),
            PathAndQuery::from_static(WRITE),
            ProstCodec::<WriteRequest, WriteResponse>::default(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_digests_and_functions_are_refused() {
    let server = start().await;
    let mut zero = digest(b"not empty");
    zero.size_bytes = 0;
    let blake3 = GetActionResultRequest {
        digest_function: 9,
        ..get("", b"act")
    };
    let zero_size = GetActionResultRequest {
        action_digest: Some(zero),
        ..get("", b"act")
    };

    for request in [blake3, zero_size] {
        let error = unary::<_, FullResult>(&server, GET_RESULT, request, None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn credentials_are_checked_on_every_call() {
    let server = start_with(Some("writer *:write\nreader reapi:read\n")).await;
    let basic = |token: &str| {
        use base64::Engine as _;
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("ci:{token}"))
        )
    };

    let anonymous = write_blob(&server, "", b"x", None).await.unwrap_err();
    assert_eq!(anonymous.code(), tonic::Code::Unauthenticated);
    let reader = write_blob(&server, "", b"x", Some(&basic("reader")))
        .await
        .unwrap_err();
    assert_eq!(reader.code(), tonic::Code::PermissionDenied);
    write_blob(&server, "", b"x", Some(&basic("writer")))
        .await
        .unwrap();

    let found: FindMissingBlobsResponse = unary(
        &server,
        FIND_MISSING,
        FindMissingBlobsRequest {
            instance_name: String::new(),
            blob_digests: vec![digest(b"x")],
            digest_function: DIGEST_SHA256,
        },
        Some(&basic("reader")),
    )
    .await
    .unwrap();
    assert_eq!(found.missing_blob_digests, []);
}

#[tokio::test(flavor = "multi_thread")]
async fn unserved_methods_are_unimplemented() {
    let server = start().await;
    let error = unary::<_, GetCapabilitiesRequest>(
        &server,
        "/build.bazel.remote.execution.v2.Execution/Execute",
        GetCapabilitiesRequest::default(),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), tonic::Code::Unimplemented);
}

/// A plain HTTP/1.1 PUT, for uploading through another front-end.
async fn reqwest_put(addr: std::net::SocketAddr, path: &str, body: &[u8]) -> u16 {
    use hyper_util::rt::TokioIo;
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(connection);
    let request = http::Request::put(path)
        .header(http::header::HOST, addr.to_string())
        .body(http_body_util::Full::new(Bytes::copy_from_slice(body)))
        .unwrap();
    sender
        .send_request(request)
        .await
        .unwrap()
        .status()
        .as_u16()
}
