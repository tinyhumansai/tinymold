//! The REAPI calls tinymold makes, over one gRPC channel.
//!
//! Each call is written out by hand: a [`tonic::client::Grpc`] over a lazily
//! connected [`Channel`], the method's path, and a prost codec for its two
//! messages. That is all generated code would do, without `protoc`.
//!
//! Every call carries the configured `authorization` header and runs under a
//! timeout. Blob transfers get more time in proportion to their size, so a
//! large binary on a slow link does not time out while it is still moving.

use std::time::Duration;

use tonic::client::Grpc;
use http::uri::PathAndQuery;
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Code, Request};
use tonic_prost::ProstCodec;

use crate::config::RemoteConfig;
use crate::naming;
use crate::proto::{
    ActionResult, DIGEST_SHA256, Digest, FindMissingBlobsRequest, FindMissingBlobsResponse,
    GetActionResultRequest, GetCapabilitiesRequest, ReadRequest, ReadResponse,
    ServerCapabilities, UpdateActionResultRequest, WriteRequest, WriteResponse,
};
use crate::{Error, Result};

/// How large one `ByteStream` write chunk is.
const CHUNK: usize = 1 << 20;

/// The transfer rate a blob's extra time allowance assumes: 4 MiB/s.
const SLOWEST_RATE: u64 = 4 << 20;

/// The largest message the client accepts: one chunk with plenty of slack,
/// or a large `FindMissingBlobs` answer.
const MAX_MESSAGE: usize = 64 << 20;

/// A connection to one REAPI server.
#[derive(Debug, Clone)]
pub(crate) struct Client {
    grpc: Grpc<Channel>,
    authorization: Option<MetadataValue<tonic::metadata::Ascii>>,
    instance: String,
    timeout: Duration,
}

impl Client {
    /// A client for `config`. The channel connects on its first call; this
    /// must run inside the tokio runtime the calls will run on.
    pub(crate) fn new(config: &RemoteConfig) -> Result<Self> {
        let mut endpoint = Endpoint::from_shared(config.endpoint.clone())
            .map_err(Error::Connect)?
            .connect_timeout(config.timeout);
        if config.uses_tls() {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_webpki_roots())
                .map_err(Error::Connect)?;
        }
        let authorization = config
            .authorization
            .as_deref()
            .map(|value| value.parse().map_err(|_| Error::InvalidAuth))
            .transpose()?;
        Ok(Self {
            grpc: Grpc::new(endpoint.connect_lazy()).max_decoding_message_size(MAX_MESSAGE),
            authorization,
            instance: config.instance.clone(),
            timeout: config.timeout,
        })
    }

    /// What the server supports.
    pub(crate) async fn capabilities(&self) -> Result<ServerCapabilities> {
        let request = GetCapabilitiesRequest {
            instance_name: self.instance.clone(),
        };
        self.unary(
            "GetCapabilities",
            "/build.bazel.remote.execution.v2.Capabilities/GetCapabilities",
            request,
        )
        .await
    }

    /// The `ActionResult` under `key`, or `None` when there is none.
    pub(crate) async fn get_action_result(&self, key: Digest) -> Result<Option<ActionResult>> {
        let request = GetActionResultRequest {
            instance_name: self.instance.clone(),
            action_digest: Some(key),
            digest_function: DIGEST_SHA256,
        };
        match self
            .unary(
                "GetActionResult",
                "/build.bazel.remote.execution.v2.ActionCache/GetActionResult",
                request,
            )
            .await
        {
            Ok(result) => Ok(Some(result)),
            Err(Error::Status { status, .. }) if status.code() == Code::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Stores `result` under `key`.
    pub(crate) async fn update_action_result(&self, key: Digest, result: ActionResult) -> Result<()> {
        let request = UpdateActionResultRequest {
            instance_name: self.instance.clone(),
            action_digest: Some(key),
            action_result: Some(result),
            digest_function: DIGEST_SHA256,
        };
        let _: ActionResult = self
            .unary(
                "UpdateActionResult",
                "/build.bazel.remote.execution.v2.ActionCache/UpdateActionResult",
                request,
            )
            .await?;
        Ok(())
    }

    /// Which of `digests` the server does not hold.
    pub(crate) async fn find_missing(&self, digests: Vec<Digest>) -> Result<Vec<Digest>> {
        let request = FindMissingBlobsRequest {
            instance_name: self.instance.clone(),
            blob_digests: digests,
            digest_function: DIGEST_SHA256,
        };
        let response: FindMissingBlobsResponse = self
            .unary(
                "FindMissingBlobs",
                "/build.bazel.remote.execution.v2.ContentAddressableStorage/FindMissingBlobs",
                request,
            )
            .await?;
        Ok(response.missing_blob_digests)
    }

    /// Reads the blob `digest`, checking that its content hashes to it.
    pub(crate) async fn read(&self, digest: &Digest) -> Result<Vec<u8>> {
        const CALL: &str = "Read";
        let request = ReadRequest {
            resource_name: naming::read_resource(&self.instance, digest),
            read_offset: 0,
            read_limit: 0,
        };
        let transfer = async {
            let mut grpc = self.ready(CALL).await?;
            let mut stream = grpc
                .server_streaming(
                    self.request(request),
                    PathAndQuery::from_static("/google.bytestream.ByteStream/Read"),
                    ProstCodec::<ReadRequest, ReadResponse>::default(),
                )
                .await
                .map_err(status(CALL))?
                .into_inner();
            let mut data = Vec::with_capacity(usize::try_from(digest.size_bytes).unwrap_or(0));
            while let Some(chunk) = stream.message().await.map_err(status(CALL))? {
                data.extend_from_slice(&chunk.data);
            }
            Ok(data)
        };
        let data = self.within(CALL, self.allowance(digest), transfer).await?;
        if naming::blob(&data) == *digest {
            Ok(data)
        } else {
            Err(Error::BlobMismatch {
                expected: digest.hash.clone(),
            })
        }
    }

    /// Uploads `data`, whose digest is `digest`.
    pub(crate) async fn write(&self, digest: &Digest, data: Vec<u8>) -> Result<()> {
        const CALL: &str = "Write";
        let resource = naming::write_resource(&self.instance, digest);
        let mut chunks = Vec::with_capacity(data.len().div_ceil(CHUNK).max(1));
        let mut offset = 0;
        loop {
            let end = (offset + CHUNK).min(data.len());
            chunks.push(WriteRequest {
                // Servers read the resource name from the first chunk only.
                resource_name: if offset == 0 {
                    resource.clone()
                } else {
                    String::new()
                },
                write_offset: i64::try_from(offset).unwrap_or(i64::MAX),
                finish_write: end == data.len(),
                data: data[offset..end].to_vec(),
            });
            offset = end;
            if offset == data.len() {
                break;
            }
        }
        let transfer = async {
            let mut grpc = self.ready(CALL).await?;
            grpc.client_streaming(
                self.request(tokio_stream::iter(chunks)),
                PathAndQuery::from_static("/google.bytestream.ByteStream/Write"),
                ProstCodec::<WriteRequest, WriteResponse>::default(),
            )
            .await
            .map_err(status(CALL))
        };
        self.within(CALL, self.allowance(digest), transfer).await?;
        Ok(())
    }

    async fn unary<Req, Resp>(&self, call: &'static str, path: &'static str, message: Req) -> Result<Resp>
    where
        Req: prost::Message + Send + Sync + 'static,
        Resp: prost::Message + Default + Send + Sync + 'static,
    {
        let exchange = async {
            let mut grpc = self.ready(call).await?;
            grpc.unary(
                self.request(message),
                PathAndQuery::from_static(path),
                ProstCodec::<Req, Resp>::default(),
            )
            .await
            .map(tonic::Response::into_inner)
            .map_err(status(call))
        };
        self.within(call, self.timeout, exchange).await
    }

    /// A clone of the channel once it can take a call.
    async fn ready(&self, call: &'static str) -> Result<Grpc<Channel>> {
        let mut grpc = self.grpc.clone();
        grpc.ready().await.map_err(|error| Error::Status {
            call,
            status: Box::new(tonic::Status::unavailable(error.to_string())),
        })?;
        Ok(grpc)
    }

    /// `message` as a request carrying the credential.
    fn request<T>(&self, message: T) -> Request<T> {
        let mut request = Request::new(message);
        if let Some(value) = &self.authorization {
            request
                .metadata_mut()
                .insert("authorization", value.clone());
        }
        request
    }

    /// The timeout plus a second for every [`SLOWEST_RATE`] bytes of blob.
    fn allowance(&self, digest: &Digest) -> Duration {
        let bytes = u64::try_from(digest.size_bytes).unwrap_or(0);
        self.timeout + Duration::from_secs(bytes / SLOWEST_RATE)
    }

    async fn within<T>(
        &self,
        call: &'static str,
        limit: Duration,
        work: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        tokio::time::timeout(limit, work)
            .await
            .map_err(|_| Error::Timeout(call))?
    }
}

fn status(call: &'static str) -> impl Fn(tonic::Status) -> Error {
    move |status| Error::Status {
        call,
        status: Box::new(status),
    }
}

#[cfg(test)]
mod test;
