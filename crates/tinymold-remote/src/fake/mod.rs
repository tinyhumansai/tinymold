//! An in-process REAPI cache server for the tests: an `ActionCache` and a CAS
//! in memory, served over real gRPC on a local port.
//!
//! It answers the calls the client makes the way bazel-remote does: missing
//! entries are `NOT_FOUND`, blobs are checked against their SHA-256 on
//! upload, and a digest of size 0 is refused unless it is the empty blob's. Switches on [`State`] make it require a credential, refuse
//! writes, corrupt a blob, or stall, so the failure paths can be tested.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::future::{Future, ready};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use tonic::body::Body;
use tonic::server::Grpc;
use tonic::{Request, Response, Status, Streaming};
use tonic_prost::ProstCodec;

use crate::naming;
use crate::proto::{
    ActionCacheUpdateCapabilities, ActionResult, CacheCapabilities, DIGEST_SHA256,
    FindMissingBlobsRequest, FindMissingBlobsResponse, GetActionResultRequest,
    GetCapabilitiesRequest, ReadRequest, ReadResponse, ServerCapabilities,
    UpdateActionResultRequest, WriteRequest, WriteResponse,
};

/// What the fake server holds and how it misbehaves.
#[derive(Debug, Default)]
pub(crate) struct State {
    /// `ActionCache` entries by key hash.
    pub(crate) actions: HashMap<String, ActionResult>,
    /// Blobs by SHA-256 hash.
    pub(crate) blobs: HashMap<String, Vec<u8>>,
    /// The `authorization` header every call must carry, if any.
    pub(crate) required_auth: Option<String>,
    /// Whether writes are refused with `PERMISSION_DENIED`.
    pub(crate) read_only: bool,
    /// Blobs served with their content changed.
    pub(crate) corrupt: HashSet<String>,
    /// How long every call waits before answering.
    pub(crate) delay: Option<Duration>,
    /// How many blobs were uploaded.
    pub(crate) writes: u64,
}

/// A running fake server; it stops when dropped.
pub(crate) struct Server {
    pub(crate) state: Arc<Mutex<State>>,
    pub(crate) address: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    /// Starts a server on a free local port.
    pub(crate) fn start() -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (bound, address) = std::sync::mpsc::channel();
        let service = Service {
            state: Arc::clone(&state),
        };
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                bound.send(listener.local_addr().unwrap()).unwrap();
                tonic::transport::Server::builder()
                    .serve_with_incoming_shutdown(
                        service,
                        tokio_stream::wrappers::TcpListenerStream::new(listener),
                        async {
                            let _ = stopped.await;
                        },
                    )
                    .await
                    .unwrap();
            });
        });
        Self {
            state,
            address: address.recv().unwrap(),
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    /// The URL a client reaches this server at.
    pub(crate) fn url(&self) -> String {
        format!("grpc://{}", self.address)
    }

    /// Runs `change` on the server's state.
    pub(crate) fn with<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        change(&mut self.state.lock().unwrap())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// bazel-remote's `validateHash`: size 0 is only valid for the empty blob.
fn valid(digest: &crate::proto::Digest) -> Result<(), Status> {
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    if digest.size_bytes == 0 && digest.hash != EMPTY {
        return Err(Status::invalid_argument("Invalid zero-length SHA256 hash"));
    }
    Ok(())
}

#[derive(Clone)]
struct Service {
    state: Arc<Mutex<State>>,
}

type BoxFuture = Pin<Box<dyn Future<Output = Result<http::Response<Body>, Infallible>> + Send>>;

impl tower_service::Service<http::Request<Body>> for Service {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> BoxFuture {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let (required, delay) = {
                let state = state.lock().unwrap();
                (state.required_auth.clone(), state.delay)
            };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            if let Some(required) = required {
                let given = request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok());
                if given != Some(required.as_str()) {
                    return Ok(Status::unauthenticated("bad credential").into_http());
                }
            }
            Ok(route(state, request).await)
        })
    }
}

impl tonic::server::NamedService for Service {
    const NAME: &'static str = "fake.reapi";
}

async fn route(state: Arc<Mutex<State>>, request: http::Request<Body>) -> http::Response<Body> {
    let path = request.uri().path().to_owned();
    match path.as_str() {
        "/build.bazel.remote.execution.v2.Capabilities/GetCapabilities" => {
            capabilities(state, request).await
        }
        "/build.bazel.remote.execution.v2.ActionCache/GetActionResult" => {
            get_action_result(state, request).await
        }
        "/build.bazel.remote.execution.v2.ActionCache/UpdateActionResult" => {
            update_action_result(state, request).await
        }
        "/build.bazel.remote.execution.v2.ContentAddressableStorage/FindMissingBlobs" => {
            find_missing_blobs(state, request).await
        }
        "/google.bytestream.ByteStream/Read" => read(state, request).await,
        "/google.bytestream.ByteStream/Write" => write(state, request).await,
        _ => Status::unimplemented(path).into_http(),
    }
}

async fn capabilities(
    state: Arc<Mutex<State>>,
    request: http::Request<Body>,
) -> http::Response<Body> {
    let service = tower::service_fn(move |_: Request<GetCapabilitiesRequest>| {
        let read_only = state.lock().unwrap().read_only;
        ready(Ok::<_, Status>(Response::new(ServerCapabilities {
            cache_capabilities: Some(CacheCapabilities {
                digest_functions: vec![DIGEST_SHA256],
                action_cache_update_capabilities: Some(ActionCacheUpdateCapabilities {
                    update_enabled: !read_only,
                }),
            }),
        })))
    });
    Grpc::new(ProstCodec::<ServerCapabilities, GetCapabilitiesRequest>::default())
        .unary(service, request)
        .await
}

async fn get_action_result(
    state: Arc<Mutex<State>>,
    request: http::Request<Body>,
) -> http::Response<Body> {
    let service = tower::service_fn(move |call: Request<GetActionResultRequest>| {
        let key = call.into_inner().action_digest.unwrap_or_default();
        if let Err(status) = valid(&key) {
            return ready(Err(status));
        }
        let key = key.hash;
        let found = state.lock().unwrap().actions.get(&key).cloned();
        ready(
            found
                .map(Response::new)
                .ok_or_else(|| Status::not_found(key)),
        )
    });
    Grpc::new(ProstCodec::<ActionResult, GetActionResultRequest>::default())
        .unary(service, request)
        .await
}

async fn update_action_result(
    state: Arc<Mutex<State>>,
    request: http::Request<Body>,
) -> http::Response<Body> {
    let service = tower::service_fn(move |call: Request<UpdateActionResultRequest>| {
        let call = call.into_inner();
        let mut state = state.lock().unwrap();
        if state.read_only {
            return ready(Err(Status::permission_denied("read only")));
        }
        let result = call.action_result.unwrap_or_default();
        let missing = result.output_files.iter().any(|file| {
            !state
                .blobs
                .contains_key(&file.digest.clone().unwrap_or_default().hash)
        });
        if missing {
            return ready(Err(Status::failed_precondition("missing blobs")));
        }
        let key = call.action_digest.unwrap_or_default();
        if let Err(status) = valid(&key) {
            return ready(Err(status));
        }
        state.actions.insert(key.hash, result.clone());
        ready(Ok(Response::new(result)))
    });
    Grpc::new(ProstCodec::<ActionResult, UpdateActionResultRequest>::default())
        .unary(service, request)
        .await
}

async fn find_missing_blobs(
    state: Arc<Mutex<State>>,
    request: http::Request<Body>,
) -> http::Response<Body> {
    let service = tower::service_fn(move |call: Request<FindMissingBlobsRequest>| {
        let state = state.lock().unwrap();
        let missing = call
            .into_inner()
            .blob_digests
            .into_iter()
            .filter(|digest| !state.blobs.contains_key(&digest.hash))
            .collect();
        ready(Ok::<_, Status>(Response::new(FindMissingBlobsResponse {
            missing_blob_digests: missing,
        })))
    });
    Grpc::new(ProstCodec::<
        FindMissingBlobsResponse,
        FindMissingBlobsRequest,
    >::default())
    .unary(service, request)
    .await
}

async fn read(state: Arc<Mutex<State>>, request: http::Request<Body>) -> http::Response<Body> {
    let service = tower::service_fn(move |call: Request<ReadRequest>| {
        let resource = call.into_inner().resource_name;
        let hash = resource.split('/').rev().nth(1).unwrap_or("").to_owned();
        let state = state.lock().unwrap();
        let Some(mut data) = state.blobs.get(&hash).cloned() else {
            return ready(Err(Status::not_found(hash)));
        };
        if state.corrupt.contains(&hash) {
            data.push(b'!');
        }
        // Two chunks, so the client has to put a stream together.
        let middle = data.len() / 2;
        let chunks = vec![
            Ok(ReadResponse {
                data: data[..middle].to_vec(),
            }),
            Ok(ReadResponse {
                data: data[middle..].to_vec(),
            }),
        ];
        ready(Ok(Response::new(tokio_stream::iter(chunks))))
    });
    Grpc::new(ProstCodec::<ReadResponse, ReadRequest>::default())
        .server_streaming(service, request)
        .await
}

async fn write(state: Arc<Mutex<State>>, request: http::Request<Body>) -> http::Response<Body> {
    let service = tower::service_fn(move |call: Request<Streaming<WriteRequest>>| {
        let state = Arc::clone(&state);
        async move {
            let mut stream = call.into_inner();
            let mut resource = String::new();
            let mut data = Vec::new();
            while let Some(chunk) = stream.message().await? {
                if resource.is_empty() {
                    resource = chunk.resource_name;
                }
                data.extend_from_slice(&chunk.data);
            }
            let mut state = state.lock().unwrap();
            if state.read_only {
                return Err(Status::permission_denied("read only"));
            }
            let hash = resource.split('/').rev().nth(1).unwrap_or("").to_owned();
            if naming::blob(&data).hash != hash {
                return Err(Status::invalid_argument("digest mismatch"));
            }
            let size = i64::try_from(data.len()).unwrap();
            state.blobs.insert(hash, data);
            state.writes += 1;
            Ok(Response::new(WriteResponse {
                committed_size: size,
            }))
        }
    });
    Grpc::new(ProstCodec::<WriteResponse, WriteRequest>::default())
        .client_streaming(service, request)
        .await
}
