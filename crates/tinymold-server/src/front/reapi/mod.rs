//! The Remote Execution API v2 cache services, over gRPC.
//!
//! Served: `Capabilities.GetCapabilities`, `ActionCache` (get and update),
//! `ContentAddressableStorage` (`FindMissingBlobs`, `BatchUpdateBlobs`,
//! `BatchReadBlobs`) and `google.bytestream.ByteStream` (`Read`, `Write`).
//! Anything else, including `Execution`, answers `UNIMPLEMENTED`.
//!
//! - **Digests are SHA-256.** Blobs are found through the store's SHA-256
//!   index, so a blob uploaded through any front-end is visible here.
//! - **The instance name is the tenant**, for action results and for
//!   access checks.
//! - **Action results are stored as the client sent them.** An update
//!   names every blob the result depends on (output files, the files of
//!   each output directory's `Tree`, stdout and stderr), and is refused with
//!   `FAILED_PRECONDITION` unless all of them are stored, as bazel-remote
//!   does. Those blobs are the entry's references, so the collector keeps
//!   them as long as the result. A read whose blobs have since gone is
//!   `NOT_FOUND`.
//! - Credentials travel in the `authorization` metadata, as for HTTP.

mod types;

use std::io::{self, Read as _, Seek as _};
use std::str::FromStr as _;
use std::sync::Arc;

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::BodyExt as _;
use prost::Message as _;
use tinymold_store::{Claim, Entry, FrontEnd, Namespace, Sha256, Store};
use tonic::metadata::MetadataMap;
use tonic::server::Grpc;
use tonic::{Status, Streaming};
use tonic_prost::ProstCodec;
use tower::service_fn;

use crate::auth::{Denied, Permission};
use crate::metrics::Outcome;
use crate::route::{Body, State};
use crate::transfer;
use types::{
    ActionCacheUpdateCapabilities, ActionResultRefs, BatchReadBlobsRequest,
    BatchReadBlobsResponse, BatchUpdateBlobsRequest, BatchUpdateBlobsResponse, CacheCapabilities,
    DIGEST_SHA256, DIGEST_UNKNOWN, Digest, FindMissingBlobsRequest, FindMissingBlobsResponse,
    GetActionResultRequest, GetCapabilitiesRequest, Raw, ReadBlobResult, ReadRequest,
    ReadResponse, RpcStatus, SemVer, ServerCapabilities, Tree, UpdateActionResultRequest,
    UpdateBlobResult, WriteRequest, WriteResponse,
};

/// The most bytes one batch call may carry, as advertised to clients.
const MAX_BATCH: i64 = 4 << 20;

/// How much of a blob one `ByteStream.Read` message carries.
const READ_CHUNK: usize = 256 * 1024;

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Whether `request` is a gRPC call.
pub(crate) fn is_grpc<B>(request: &Request<B>) -> bool {
    request
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/grpc"))
}

/// Answers one gRPC call.
pub(crate) async fn handle<B>(state: &Arc<State>, request: Request<B>) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let state = state.clone();
    let path = request.uri().path().to_owned();
    // One concrete body type for every call, as tonic's generated servers
    // use.
    let request = request.map(tonic::body::Body::new);
    let response = match path.as_str() {
        "/build.bazel.remote.execution.v2.Capabilities/GetCapabilities" => {
            Grpc::new(ProstCodec::<ServerCapabilities, GetCapabilitiesRequest>::default())
                .unary(service_fn(|_| async { Ok(tonic::Response::new(capabilities())) }), request)
                .await
        }
        "/build.bazel.remote.execution.v2.ActionCache/GetActionResult" => {
            let service = service_fn(move |call| get_action_result(state.clone(), call));
            Grpc::new(ProstCodec::<Raw, GetActionResultRequest>::default())
                .unary(service, request)
                .await
        }
        "/build.bazel.remote.execution.v2.ActionCache/UpdateActionResult" => {
            let service = service_fn(move |call| update_action_result(state.clone(), call));
            Grpc::new(ProstCodec::<Raw, UpdateActionResultRequest>::default())
                .unary(service, request)
                .await
        }
        "/build.bazel.remote.execution.v2.ContentAddressableStorage/FindMissingBlobs" => {
            let service = service_fn(move |call| find_missing_blobs(state.clone(), call));
            Grpc::new(ProstCodec::<FindMissingBlobsResponse, FindMissingBlobsRequest>::default())
                .unary(service, request)
                .await
        }
        "/build.bazel.remote.execution.v2.ContentAddressableStorage/BatchUpdateBlobs" => {
            let service = service_fn(move |call| batch_update_blobs(state.clone(), call));
            Grpc::new(ProstCodec::<BatchUpdateBlobsResponse, BatchUpdateBlobsRequest>::default())
                .unary(service, request)
                .await
        }
        "/build.bazel.remote.execution.v2.ContentAddressableStorage/BatchReadBlobs" => {
            let service = service_fn(move |call| batch_read_blobs(state.clone(), call));
            Grpc::new(ProstCodec::<BatchReadBlobsResponse, BatchReadBlobsRequest>::default())
                .unary(service, request)
                .await
        }
        "/google.bytestream.ByteStream/Read" => {
            let service = service_fn(move |call| read(state.clone(), call));
            Grpc::new(ProstCodec::<ReadResponse, ReadRequest>::default())
                .server_streaming(service, request)
                .await
        }
        "/google.bytestream.ByteStream/Write" => {
            let service = service_fn(move |call| write(state.clone(), call));
            Grpc::new(ProstCodec::<WriteResponse, WriteRequest>::default())
                .client_streaming(service, request)
                .await
        }
        _ => Status::unimplemented(format!("{path} is not served")).into_http(),
    };
    response.map(|body| {
        body.map_err(|status| io::Error::other(status.message().to_owned()))
            .boxed()
    })
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        cache_capabilities: Some(CacheCapabilities {
            digest_functions: vec![DIGEST_SHA256],
            action_cache_update_capabilities: Some(ActionCacheUpdateCapabilities {
                update_enabled: true,
            }),
            max_batch_total_size_bytes: MAX_BATCH,
        }),
        low_api_version: Some(SemVer { major: 2, minor: 0 }),
        high_api_version: Some(SemVer { major: 2, minor: 3 }),
    }
}

async fn get_action_result(
    state: Arc<State>,
    call: tonic::Request<GetActionResultRequest>,
) -> Result<tonic::Response<Raw>, Status> {
    let (metadata, _, request) = call.into_parts();
    check_function(request.digest_function)?;
    let namespace = authorize(&state, &metadata, &request.instance_name, Permission::Read)?;
    let key = action_key(&namespace, request.action_digest.as_ref())?;

    let store = state.store.clone();
    let found = blocking(move || {
        let Some(entry) = store.get(&key).map_err(internal)? else {
            return Ok(None);
        };
        // Every blob the result names must still be here, or it is no use.
        let complete = entry.blobs.iter().all(|blob| store.cas().contains(blob));
        Ok(complete.then_some(entry.body))
    })
    .await?;

    match found {
        Some(body) => {
            record(&state, Outcome::Hit, 0, body.len());
            Ok(tonic::Response::new(Raw(body)))
        }
        None => {
            record(&state, Outcome::Miss, 0, 0);
            Err(Status::not_found("no action result"))
        }
    }
}

async fn update_action_result(
    state: Arc<State>,
    call: tonic::Request<UpdateActionResultRequest>,
) -> Result<tonic::Response<Raw>, Status> {
    let (metadata, _, request) = call.into_parts();
    check_function(request.digest_function)?;
    let namespace = authorize(&state, &metadata, &request.instance_name, Permission::Write)?;
    let key = action_key(&namespace, request.action_digest.as_ref())?;
    let raw = request.action_result;
    let refs = ActionResultRefs::decode(raw.as_slice())
        .map_err(|_| Status::invalid_argument("action_result does not decode"))?;

    let store = state.store.clone();
    let body = raw.clone();
    blocking(move || {
        let blobs = references(&store, &refs)?;
        let mut entry = Entry::new(blobs);
        entry.body = body;
        entry.written = now();
        store.put(&key, &entry).map_err(internal)
    })
    .await?;

    record(&state, Outcome::Write, raw.len(), 0);
    Ok(tonic::Response::new(Raw(raw)))
}

/// The stored objects an action result depends on, or `FAILED_PRECONDITION`
/// naming how many are missing.
fn references(store: &Store, refs: &ActionResultRefs) -> Result<Vec<tinymold_store::Digest>, Status> {
    let mut digests: Vec<&Digest> = refs
        .output_files
        .iter()
        .filter_map(|file| file.digest.as_ref())
        .chain(refs.stdout_digest.iter())
        .chain(refs.stderr_digest.iter())
        .collect();
    let trees: Vec<&Digest> = refs
        .output_directories
        .iter()
        .filter_map(|dir| dir.tree_digest.as_ref())
        .collect();
    digests.extend(trees.iter().copied());

    let mut blobs = Vec::new();
    let mut missing = 0_usize;
    let mut tree_files = Vec::new();
    for digest in digests {
        match resolve(store, digest)? {
            Some(blob) => {
                if trees.contains(&digest) {
                    let bytes = store.cas().read(&blob).map_err(internal)?;
                    let tree = Tree::decode(bytes.as_slice())
                        .map_err(|_| Status::invalid_argument("an output tree does not decode"))?;
                    for directory in tree.root.iter().chain(&tree.children) {
                        tree_files.extend(directory.files.iter().filter_map(|f| f.digest.clone()));
                    }
                }
                blobs.push(blob);
            }
            None => missing += 1,
        }
    }
    for digest in &tree_files {
        match resolve(store, digest)? {
            Some(blob) => blobs.push(blob),
            None => missing += 1,
        }
    }
    if missing > 0 {
        return Err(Status::failed_precondition(format!(
            "{missing} blobs the action result references are not stored"
        )));
    }
    blobs.sort_unstable();
    blobs.dedup();
    Ok(blobs)
}

async fn find_missing_blobs(
    state: Arc<State>,
    call: tonic::Request<FindMissingBlobsRequest>,
) -> Result<tonic::Response<FindMissingBlobsResponse>, Status> {
    let (metadata, _, request) = call.into_parts();
    check_function(request.digest_function)?;
    authorize(&state, &metadata, &request.instance_name, Permission::Read)?;
    let store = state.store.clone();
    let missing = blocking(move || {
        let mut missing = Vec::new();
        for digest in request.blob_digests {
            if digest.hash != EMPTY_SHA256 && find(&store, &digest)?.is_none() {
                missing.push(digest);
            }
        }
        Ok(missing)
    })
    .await?;
    record(&state, Outcome::Other, 0, 0);
    Ok(tonic::Response::new(FindMissingBlobsResponse {
        missing_blob_digests: missing,
    }))
}

async fn batch_update_blobs(
    state: Arc<State>,
    call: tonic::Request<BatchUpdateBlobsRequest>,
) -> Result<tonic::Response<BatchUpdateBlobsResponse>, Status> {
    let (metadata, _, request) = call.into_parts();
    check_function(request.digest_function)?;
    authorize(&state, &metadata, &request.instance_name, Permission::Write)?;
    let received: usize = request.requests.iter().map(|blob| blob.data.len()).sum();
    let store = state.store.clone();
    let responses = blocking(move || {
        let mut responses = Vec::new();
        for blob in request.requests {
            let digest = blob.digest.unwrap_or_default();
            let status = if blob.compressor != 0 {
                rpc_status(tonic::Code::InvalidArgument, "compressed uploads are not accepted")
            } else {
                match sha256(&digest) {
                    Err(status) => rpc_status(status.code(), status.message()),
                    Ok(claimed) => match store.ingest(blob.data.as_slice(), Some(Claim::Sha256(claimed))) {
                        Ok(_) => rpc_status(tonic::Code::Ok, ""),
                        Err(tinymold_store::Error::DigestMismatch { .. }) => {
                            rpc_status(tonic::Code::InvalidArgument, "data does not match its digest")
                        }
                        Err(_) => rpc_status(tonic::Code::Internal, "store failed"),
                    },
                }
            };
            responses.push(UpdateBlobResult {
                digest: Some(digest),
                status: Some(status),
            });
        }
        Ok(responses)
    })
    .await?;
    record(&state, Outcome::Write, received, 0);
    Ok(tonic::Response::new(BatchUpdateBlobsResponse { responses }))
}

async fn batch_read_blobs(
    state: Arc<State>,
    call: tonic::Request<BatchReadBlobsRequest>,
) -> Result<tonic::Response<BatchReadBlobsResponse>, Status> {
    let (metadata, _, request) = call.into_parts();
    check_function(request.digest_function)?;
    authorize(&state, &metadata, &request.instance_name, Permission::Read)?;
    let store = state.store.clone();
    let responses = blocking(move || {
        let mut responses = Vec::new();
        for digest in request.digests {
            let (data, status) = match resolve(&store, &digest) {
                Ok(Some(blob)) => match store.cas().read(&blob) {
                    Ok(data) => (data, rpc_status(tonic::Code::Ok, "")),
                    Err(_) => (Vec::new(), rpc_status(tonic::Code::NotFound, "blob is gone")),
                },
                Ok(None) => (Vec::new(), rpc_status(tonic::Code::NotFound, "blob is not stored")),
                Err(status) => (Vec::new(), rpc_status(status.code(), status.message())),
            };
            responses.push(ReadBlobResult {
                digest: Some(digest),
                data,
                status: Some(status),
            });
        }
        Ok(responses)
    })
    .await?;
    let sent = responses.iter().map(|blob| blob.data.len()).sum();
    let outcome = if responses
        .iter()
        .all(|blob| blob.status.as_ref().is_some_and(|status| status.code == 0))
    {
        Outcome::Hit
    } else {
        Outcome::Miss
    };
    record(&state, outcome, 0, sent);
    Ok(tonic::Response::new(BatchReadBlobsResponse { responses }))
}

type ReadStream = tokio_stream::wrappers::ReceiverStream<Result<ReadResponse, Status>>;

async fn read(
    state: Arc<State>,
    call: tonic::Request<ReadRequest>,
) -> Result<tonic::Response<ReadStream>, Status> {
    let (metadata, _, request) = call.into_parts();
    let (instance, digest) = read_resource(&request.resource_name)?;
    authorize(&state, &metadata, instance, Permission::Read)?;
    let offset = u64::try_from(request.read_offset)
        .map_err(|_| Status::out_of_range("negative read offset"))?;
    let limit = u64::try_from(request.read_limit)
        .map_err(|_| Status::invalid_argument("negative read limit"))?;

    let store = state.store.clone();
    let opened = blocking(move || {
        let Some(blob) = resolve(&store, &digest)? else {
            return Ok(None);
        };
        let Ok(mut file) = std::fs::File::open(store.cas().object_path(&blob)) else {
            return Ok(None);
        };
        let len = file.metadata().map_err(internal)?.len();
        if offset > len {
            return Err(Status::out_of_range("read offset is past the end of the blob"));
        }
        file.seek(io::SeekFrom::Start(offset)).map_err(internal)?;
        let remaining = if limit == 0 { len - offset } else { limit.min(len - offset) };
        Ok(Some((file, remaining)))
    })
    .await?;

    let Some((file, remaining)) = opened else {
        record(&state, Outcome::Miss, 0, 0);
        return Err(Status::not_found("blob is not stored"));
    };
    record(&state, Outcome::Hit, 0, usize::try_from(remaining).unwrap_or(usize::MAX));
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    tokio::task::spawn_blocking(move || stream_blob(file, remaining, &sender));
    Ok(tonic::Response::new(ReadStream::new(receiver)))
}

fn stream_blob(
    file: std::fs::File,
    remaining: u64,
    sender: &tokio::sync::mpsc::Sender<Result<ReadResponse, Status>>,
) {
    let mut file = file.take(remaining);
    loop {
        let mut chunk = vec![0_u8; READ_CHUNK];
        match file.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => {
                chunk.truncate(read);
                let message = ReadResponse {
                    data: Bytes::from(chunk),
                };
                if sender.blocking_send(Ok(message)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.blocking_send(Err(internal(error)));
                return;
            }
        }
    }
}

async fn write(
    state: Arc<State>,
    call: tonic::Request<Streaming<WriteRequest>>,
) -> Result<tonic::Response<WriteResponse>, Status> {
    let (metadata, _, mut stream) = call.into_parts();
    let Some(first) = stream.message().await? else {
        return Err(Status::invalid_argument("empty write"));
    };
    let (instance, digest) = write_resource(&first.resource_name)?;
    authorize(&state, &metadata, instance, Permission::Write)?;
    let claimed = sha256(&digest)?;
    let size = u64::try_from(digest.size_bytes)
        .map_err(|_| Status::invalid_argument("negative blob size"))?;
    if size > state.max_body {
        return Err(Status::resource_exhausted("blob is larger than the server accepts"));
    }

    let (sender, mut reader) = transfer::pipe();
    let store = state.store.clone();
    let ingest = tokio::task::spawn_blocking(move || {
        store.ingest(&mut reader, Some(Claim::Sha256(claimed)))
    });

    let mut written: u64 = 0;
    let mut next = Some(first);
    let mut failure = None;
    while let Some(chunk) = next.take() {
        if u64::try_from(chunk.write_offset).ok() != Some(written) {
            failure = Some(Status::invalid_argument("write offsets must be contiguous"));
            break;
        }
        written += chunk.data.len() as u64;
        if written > size {
            failure = Some(Status::invalid_argument("more data than the digest's size"));
            break;
        }
        if sender.send(Ok(chunk.data)).await.is_err() || chunk.finish_write {
            break;
        }
        next = stream.message().await?;
    }
    if failure.is_some() {
        let _ = sender.send(Err(io::Error::other("write aborted"))).await;
    }
    drop(sender);

    let stored = ingest.await.map_err(|_| Status::internal("store task failed"))?;
    if let Some(failure) = failure {
        return Err(failure);
    }
    match stored {
        Ok(_) => {
            record(&state, Outcome::Write, usize::try_from(written).unwrap_or(usize::MAX), 0);
            Ok(tonic::Response::new(WriteResponse {
                committed_size: digest.size_bytes,
            }))
        }
        Err(tinymold_store::Error::DigestMismatch { .. }) => {
            Err(Status::invalid_argument("data does not match its digest"))
        }
        Err(error) => Err(internal(error)),
    }
}

/// Splits `[{instance}/]blobs/{hash}/{size}` into instance and digest.
fn read_resource(resource: &str) -> Result<(&str, Digest), Status> {
    let segments: Vec<&str> = resource.split('/').collect();
    let at = segments
        .iter()
        .position(|segment| *segment == "blobs")
        .ok_or_else(|| Status::invalid_argument("resource name has no blobs/ part"))?;
    let instance = instance_before(resource, &segments, at);
    Ok((instance, resource_digest(&segments[at + 1..])?))
}

/// Splits `[{instance}/]uploads/{uuid}/blobs/{hash}/{size}[/...]` into
/// instance and digest.
fn write_resource(resource: &str) -> Result<(&str, Digest), Status> {
    let segments: Vec<&str> = resource.split('/').collect();
    let at = segments
        .iter()
        .position(|segment| *segment == "uploads")
        .ok_or_else(|| Status::invalid_argument("resource name has no uploads/ part"))?;
    if segments.get(at + 2) != Some(&"blobs") {
        return Err(Status::invalid_argument(
            "only uncompressed uploads/{uuid}/blobs/ writes are accepted",
        ));
    }
    let instance = instance_before(resource, &segments, at);
    Ok((instance, resource_digest(&segments[at + 3..])?))
}

fn instance_before<'a>(resource: &'a str, segments: &[&str], at: usize) -> &'a str {
    let len: usize = segments[..at].iter().map(|segment| segment.len() + 1).sum();
    resource.get(..len.saturating_sub(1)).unwrap_or("")
}

fn resource_digest(rest: &[&str]) -> Result<Digest, Status> {
    let (Some(hash), Some(size)) = (rest.first(), rest.get(1)) else {
        return Err(Status::invalid_argument("resource name has no hash and size"));
    };
    let size_bytes = size
        .parse()
        .map_err(|_| Status::invalid_argument("resource size is not a number"))?;
    Ok(Digest {
        hash: (*hash).to_owned(),
        size_bytes,
    })
}

/// Checks `instance` exists as a namespace and the call's credential may
/// `need` it.
fn authorize(
    state: &State,
    metadata: &MetadataMap,
    instance: &str,
    need: Permission,
) -> Result<Namespace, Status> {
    let namespace = Namespace::new(FrontEnd::Reapi, instance)
        .map_err(|_| Status::invalid_argument("instance name too long"))?;
    let headers = metadata.clone().into_headers();
    state
        .access
        .check(&headers, &namespace, need)
        .map_err(|denial| {
            record(state, Outcome::Denied, 0, 0);
            match denial {
                Denied::Unauthenticated => Status::unauthenticated("a valid credential is required"),
                Denied::Forbidden => Status::permission_denied("the credential lacks this scope"),
            }
        })?;
    Ok(namespace)
}

fn check_function(function: i32) -> Result<(), Status> {
    if function == DIGEST_UNKNOWN || function == DIGEST_SHA256 {
        Ok(())
    } else {
        Err(Status::invalid_argument("only the SHA-256 digest function is served"))
    }
}

/// The store key of an action digest. A digest of size 0 is accepted only
/// for the empty blob, as bazel-remote does.
fn action_key(
    namespace: &Namespace,
    digest: Option<&Digest>,
) -> Result<tinymold_store::Key, Status> {
    let digest = digest.ok_or_else(|| Status::invalid_argument("no action digest"))?;
    let sha = sha256(digest)?;
    namespace
        .key(sha.to_hex().as_bytes())
        .map_err(|_| Status::invalid_argument("action digest too long"))
}

fn sha256(digest: &Digest) -> Result<Sha256, Status> {
    if digest.size_bytes == 0 && digest.hash != EMPTY_SHA256 {
        return Err(Status::invalid_argument("invalid zero-length SHA-256 digest"));
    }
    if digest.size_bytes < 0 {
        return Err(Status::invalid_argument("negative digest size"));
    }
    Sha256::from_str(&digest.hash).map_err(|_| Status::invalid_argument("digest hash is not SHA-256 hex"))
}

/// The stored object a digest names, storing the empty blob on demand:
/// clients never upload it.
fn resolve(store: &Store, digest: &Digest) -> Result<Option<tinymold_store::Digest>, Status> {
    if digest.hash == EMPTY_SHA256 {
        return store
            .ingest(io::empty(), None)
            .map(|ingested| Some(ingested.digest))
            .map_err(internal);
    }
    find(store, digest)
}

fn find(store: &Store, digest: &Digest) -> Result<Option<tinymold_store::Digest>, Status> {
    store.find_sha256(&sha256(digest)?).map_err(internal)
}

fn rpc_status(code: tonic::Code, message: &str) -> RpcStatus {
    RpcStatus {
        code: code as i32,
        message: message.to_owned(),
    }
}

fn internal(error: impl std::fmt::Display) -> Status {
    Status::internal(format!("store failed: {error}"))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, Status> + Send + 'static,
) -> Result<T, Status> {
    tokio::task::spawn_blocking(call)
        .await
        .unwrap_or_else(|_| Err(Status::internal("store task failed")))
}

fn record(state: &State, outcome: Outcome, bytes_in: usize, bytes_out: usize) {
    state.metrics.record(
        FrontEnd::Reapi,
        outcome,
        bytes_in as u64,
        bytes_out as u64,
    );
}

#[cfg(test)]
mod test;
