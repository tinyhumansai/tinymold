//! The REAPI v2, `ByteStream` and `google.rpc` messages the server reads and
//! writes.
//!
//! Only the fields the server uses are declared; protobuf skips the rest
//! when decoding. Field numbers follow
//! `build/bazel/remote/execution/v2/remote_execution.proto`,
//! `build/bazel/semver/semver.proto`, `google/bytestream/bytestream.proto`
//! and `google/rpc/status.proto`, and are pinned against encoded bytes in
//! the tests.
//!
//! Action results are the exception: the server keeps them as the bytes the
//! client sent ([`Raw`]), so fields this file does not declare, such as
//! Bazel's execution metadata, come back unchanged. [`ActionResultRefs`] and
//! [`Tree`] read only the digests a result depends on.

use bytes::{Buf, BufMut};
use prost::encoding::{DecodeContext, WireType};

/// `DigestFunction.Value.SHA256`.
pub(crate) const DIGEST_SHA256: i32 = 1;
/// `DigestFunction.Value.UNKNOWN`, which clients send to mean the server's
/// default, SHA-256.
pub(crate) const DIGEST_UNKNOWN: i32 = 0;

/// A message whose encoding is exactly the bytes it holds: a stored
/// `ActionResult`, sent back as it was received.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Raw(pub(crate) Vec<u8>);

impl prost::Message for Raw {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        buf.put_slice(&self.0);
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: WireType,
        buf: &mut impl Buf,
        ctx: DecodeContext,
    ) -> Result<(), prost::DecodeError> {
        // Never decoded: the server only sends raw messages.
        prost::encoding::skip_field(wire_type, tag, buf, ctx)
    }

    fn encoded_len(&self) -> usize {
        self.0.len()
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

/// A blob's name, or an action's.
#[derive(Clone, PartialEq, Eq, Hash, prost::Message)]
pub(crate) struct Digest {
    #[prost(string, tag = "1")]
    pub(crate) hash: String,
    #[prost(int64, tag = "2")]
    pub(crate) size_bytes: i64,
}

/// `google.rpc.Status`, for per-blob results of batch calls.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct RpcStatus {
    #[prost(int32, tag = "1")]
    pub(crate) code: i32,
    #[prost(string, tag = "2")]
    pub(crate) message: String,
}

/// `Capabilities.GetCapabilities`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetCapabilitiesRequest {
    #[prost(string, tag = "1")]
    pub(crate) instance_name: String,
}

/// `build.bazel.semver.SemVer`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SemVer {
    #[prost(int32, tag = "1")]
    pub(crate) major: i32,
    #[prost(int32, tag = "2")]
    pub(crate) minor: i32,
}

/// What the server supports.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ServerCapabilities {
    #[prost(message, optional, tag = "1")]
    pub(crate) cache_capabilities: Option<CacheCapabilities>,
    #[prost(message, optional, tag = "4")]
    pub(crate) low_api_version: Option<SemVer>,
    #[prost(message, optional, tag = "5")]
    pub(crate) high_api_version: Option<SemVer>,
}

/// What the cache supports.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CacheCapabilities {
    #[prost(int32, repeated, tag = "1")]
    pub(crate) digest_functions: Vec<i32>,
    #[prost(message, optional, tag = "2")]
    pub(crate) action_cache_update_capabilities: Option<ActionCacheUpdateCapabilities>,
    #[prost(int64, tag = "4")]
    pub(crate) max_batch_total_size_bytes: i64,
}

/// Whether clients may write action results.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ActionCacheUpdateCapabilities {
    #[prost(bool, tag = "1")]
    pub(crate) update_enabled: bool,
}

/// `ActionCache.GetActionResult`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetActionResultRequest {
    #[prost(string, tag = "1")]
    pub(crate) instance_name: String,
    #[prost(message, optional, tag = "2")]
    pub(crate) action_digest: Option<Digest>,
    #[prost(int32, tag = "6")]
    pub(crate) digest_function: i32,
}

/// `ActionCache.UpdateActionResult`, with the result kept as raw bytes.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct UpdateActionResultRequest {
    #[prost(string, tag = "1")]
    pub(crate) instance_name: String,
    #[prost(message, optional, tag = "2")]
    pub(crate) action_digest: Option<Digest>,
    /// An embedded `ActionResult`. Declaring it as bytes keeps every field,
    /// known or not, exactly as the client encoded it.
    #[prost(bytes = "vec", tag = "3")]
    pub(crate) action_result: Vec<u8>,
    #[prost(int32, tag = "5")]
    pub(crate) digest_function: i32,
}

/// The digests an `ActionResult` depends on.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ActionResultRefs {
    #[prost(message, repeated, tag = "2")]
    pub(crate) output_files: Vec<OutputFileRef>,
    #[prost(message, repeated, tag = "3")]
    pub(crate) output_directories: Vec<OutputDirectoryRef>,
    #[prost(message, optional, tag = "6")]
    pub(crate) stdout_digest: Option<Digest>,
    #[prost(message, optional, tag = "8")]
    pub(crate) stderr_digest: Option<Digest>,
}

/// `OutputFile`, its digest only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct OutputFileRef {
    #[prost(message, optional, tag = "2")]
    pub(crate) digest: Option<Digest>,
}

/// `OutputDirectory`, its tree's digest only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct OutputDirectoryRef {
    #[prost(message, optional, tag = "3")]
    pub(crate) tree_digest: Option<Digest>,
}

/// `Tree`: a directory and all of its descendants, inline.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Tree {
    #[prost(message, optional, tag = "1")]
    pub(crate) root: Option<Directory>,
    #[prost(message, repeated, tag = "2")]
    pub(crate) children: Vec<Directory>,
}

/// `Directory`, its files only: subdirectories are other entries of the
/// same [`Tree`].
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Directory {
    #[prost(message, repeated, tag = "1")]
    pub(crate) files: Vec<FileNode>,
}

/// `FileNode`, its digest only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct FileNode {
    #[prost(message, optional, tag = "2")]
    pub(crate) digest: Option<Digest>,
}

/// `ContentAddressableStorage.FindMissingBlobs`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct FindMissingBlobsRequest {
    #[prost(string, tag = "1")]
    pub(crate) instance_name: String,
    #[prost(message, repeated, tag = "2")]
    pub(crate) blob_digests: Vec<Digest>,
    #[prost(int32, tag = "3")]
    pub(crate) digest_function: i32,
}

/// The answer to [`FindMissingBlobsRequest`].
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct FindMissingBlobsResponse {
    #[prost(message, repeated, tag = "2")]
    pub(crate) missing_blob_digests: Vec<Digest>,
}

/// `ContentAddressableStorage.BatchUpdateBlobs`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct BatchUpdateBlobsRequest {
    #[prost(string, tag = "1")]
    pub(crate) instance_name: String,
    #[prost(message, repeated, tag = "2")]
    pub(crate) requests: Vec<UpdateBlob>,
    #[prost(int32, tag = "5")]
    pub(crate) digest_function: i32,
}

/// One blob of a [`BatchUpdateBlobsRequest`].
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct UpdateBlob {
    #[prost(message, optional, tag = "1")]
    pub(crate) digest: Option<Digest>,
    #[prost(bytes = "vec", tag = "2")]
    pub(crate) data: Vec<u8>,
    /// `Compressor.Value`; only `IDENTITY` (0) is accepted.
    #[prost(int32, tag = "3")]
    pub(crate) compressor: i32,
}

/// The answer to [`BatchUpdateBlobsRequest`].
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct BatchUpdateBlobsResponse {
    #[prost(message, repeated, tag = "1")]
    pub(crate) responses: Vec<UpdateBlobResult>,
}

/// The result for one blob of a batch update.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct UpdateBlobResult {
    #[prost(message, optional, tag = "1")]
    pub(crate) digest: Option<Digest>,
    #[prost(message, optional, tag = "2")]
    pub(crate) status: Option<RpcStatus>,
}

/// `ContentAddressableStorage.BatchReadBlobs`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct BatchReadBlobsRequest {
    #[prost(string, tag = "1")]
    pub(crate) instance_name: String,
    #[prost(message, repeated, tag = "2")]
    pub(crate) digests: Vec<Digest>,
    #[prost(int32, tag = "4")]
    pub(crate) digest_function: i32,
}

/// The answer to [`BatchReadBlobsRequest`].
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct BatchReadBlobsResponse {
    #[prost(message, repeated, tag = "1")]
    pub(crate) responses: Vec<ReadBlobResult>,
}

/// The result for one blob of a batch read.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ReadBlobResult {
    #[prost(message, optional, tag = "1")]
    pub(crate) digest: Option<Digest>,
    #[prost(bytes = "vec", tag = "2")]
    pub(crate) data: Vec<u8>,
    #[prost(message, optional, tag = "3")]
    pub(crate) status: Option<RpcStatus>,
}

/// `ByteStream.Read`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ReadRequest {
    #[prost(string, tag = "1")]
    pub(crate) resource_name: String,
    #[prost(int64, tag = "2")]
    pub(crate) read_offset: i64,
    #[prost(int64, tag = "3")]
    pub(crate) read_limit: i64,
}

/// One chunk of a read.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ReadResponse {
    #[prost(bytes = "bytes", tag = "10")]
    pub(crate) data: bytes::Bytes,
}

/// One chunk of a `ByteStream.Write`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct WriteRequest {
    #[prost(string, tag = "1")]
    pub(crate) resource_name: String,
    #[prost(int64, tag = "2")]
    pub(crate) write_offset: i64,
    #[prost(bool, tag = "3")]
    pub(crate) finish_write: bool,
    #[prost(bytes = "bytes", tag = "10")]
    pub(crate) data: bytes::Bytes,
}

/// The answer to a write.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct WriteResponse {
    #[prost(int64, tag = "1")]
    pub(crate) committed_size: i64,
}
