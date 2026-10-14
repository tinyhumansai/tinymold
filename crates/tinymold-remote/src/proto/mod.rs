//! The REAPI v2 and ByteStream messages the client sends and receives.
//!
//! Only the fields tinymold uses are declared. Protobuf skips unknown fields
//! when decoding, so a server's richer messages still parse, and a field
//! left out is simply never sent. Field numbers follow
//! `build/bazel/remote/execution/v2/remote_execution.proto`,
//! `google/bytestream/bytestream.proto` and `google/rpc/status.proto`, and
//! are pinned against encoded bytes in the tests.

/// `build.bazel.remote.execution.v2.DigestFunction.Value.SHA256`.
pub(crate) const DIGEST_SHA256: i32 = 1;
/// `build.bazel.remote.execution.v2.DigestFunction.Value.BLAKE3`.
pub(crate) const DIGEST_BLAKE3: i32 = 9;

/// A blob's name in the CAS, or a key in the ActionCache.
#[derive(Clone, PartialEq, Eq, Hash, prost::Message)]
pub(crate) struct Digest {
    /// Lowercase hex of the hash.
    #[prost(string, tag = "1")]
    pub(crate) hash: String,
    /// The blob's size; 0 for an ActionCache key.
    #[prost(int64, tag = "2")]
    pub(crate) size_bytes: i64,
}

/// One file of an action's result.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct OutputFile {
    /// The file's path relative to the action's working directory.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
    /// Its content's digest.
    #[prost(message, optional, tag = "2")]
    pub(crate) digest: Option<Digest>,
    /// Whether it is executable.
    #[prost(bool, tag = "4")]
    pub(crate) is_executable: bool,
}

/// What an action produced, as the ActionCache stores it.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ActionResult {
    /// Every output file.
    #[prost(message, repeated, tag = "2")]
    pub(crate) output_files: Vec<OutputFile>,
    /// The action's exit code; tinymold stores successes only.
    #[prost(int32, tag = "4")]
    pub(crate) exit_code: i32,
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

/// `ActionCache.UpdateActionResult`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct UpdateActionResultRequest {
    #[prost(string, tag = "1")]
    pub(crate) instance_name: String,
    #[prost(message, optional, tag = "2")]
    pub(crate) action_digest: Option<Digest>,
    #[prost(message, optional, tag = "3")]
    pub(crate) action_result: Option<ActionResult>,
    #[prost(int32, tag = "5")]
    pub(crate) digest_function: i32,
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

/// `Capabilities.GetCapabilities`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetCapabilitiesRequest {
    #[prost(string, tag = "1")]
    pub(crate) instance_name: String,
}

/// What a server supports.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ServerCapabilities {
    #[prost(message, optional, tag = "1")]
    pub(crate) cache_capabilities: Option<CacheCapabilities>,
}

/// What a server's cache supports.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CacheCapabilities {
    #[prost(int32, repeated, tag = "1")]
    pub(crate) digest_functions: Vec<i32>,
    #[prost(message, optional, tag = "2")]
    pub(crate) action_cache_update_capabilities: Option<ActionCacheUpdateCapabilities>,
}

/// Whether clients may write to the ActionCache.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ActionCacheUpdateCapabilities {
    #[prost(bool, tag = "1")]
    pub(crate) update_enabled: bool,
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

/// One chunk of a [`ReadRequest`]'s answer.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ReadResponse {
    #[prost(bytes = "vec", tag = "10")]
    pub(crate) data: Vec<u8>,
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
    #[prost(bytes = "vec", tag = "10")]
    pub(crate) data: Vec<u8>,
}

/// The answer to a `ByteStream.Write`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct WriteResponse {
    #[prost(int64, tag = "1")]
    pub(crate) committed_size: i64,
}

#[cfg(test)]
mod test;
