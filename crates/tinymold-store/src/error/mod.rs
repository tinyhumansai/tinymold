//! Error and result types for the cache store.
//!
//! Every fallible public function in this crate returns [`Result`]. Messages
//! stay lowercase without trailing punctuation.

use std::path::PathBuf;

use tinymold_hash::Digest;

use crate::Claim;

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation on an entry or index file failed.
    #[error("failed to {action} {path}")]
    Io {
        /// What was being attempted, such as `"write"` or `"rename into place"`.
        action: &'static str,
        /// The path the operation targeted.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The content-addressed store underneath failed.
    #[error(transparent)]
    Cas(#[from] tinymold_cas::Error),

    /// A tenant is longer than [`MAX_TENANT`](crate::MAX_TENANT) bytes.
    #[error("tenant is {len} bytes, more than the limit")]
    TenantTooLong {
        /// The tenant's length in bytes.
        len: usize,
    },

    /// A client key is longer than [`MAX_CLIENT_KEY`](crate::MAX_CLIENT_KEY)
    /// bytes.
    #[error("client key is {len} bytes, more than the limit")]
    KeyTooLong {
        /// The key's length in bytes.
        len: usize,
    },

    /// An entry's metadata is larger than [`MAX_META`](crate::MAX_META) bytes.
    #[error("entry metadata is {size} bytes, more than the limit")]
    MetaTooLarge {
        /// The metadata's size: the sum of every key and value length.
        size: usize,
    },

    /// Stored entry bytes are not a valid entry record.
    #[error("malformed entry record: {reason}")]
    MalformedEntry {
        /// What is wrong with it.
        reason: &'static str,
    },

    /// Text that should be a SHA-256 digest is not 64 hexadecimal digits.
    #[error("not a sha-256 digest: {0:?}")]
    InvalidSha256(String),

    /// A body does not hash to the digest its client claimed.
    #[error("body hashes to {actual}, not the claimed {claimed}")]
    DigestMismatch {
        /// The digest the client sent.
        claimed: Claim,
        /// The digest of the bytes that arrived, by the same function.
        actual: Claim,
    },

    /// An entry names an object the store does not hold.
    ///
    /// Entries are written only after every object they reference, so a
    /// reader that finds one can always serve it.
    #[error("entry references object {0}, which is not in the store")]
    MissingObject(Digest),
}

/// The crate's standard result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod test;
