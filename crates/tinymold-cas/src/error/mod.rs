//! Error and result types for the content-addressed store.
//!
//! Every fallible public function in this crate returns [`Result`]. Variants
//! carry the path or digest a caller needs to react, and messages stay
//! lowercase without trailing punctuation.

use std::path::PathBuf;

use tinymold_hash::Digest;

use crate::Method;

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation on a store or destination path failed.
    #[error("failed to {action} {path}")]
    Io {
        /// What was being attempted, such as `"create"` or `"rename into place"`.
        action: &'static str,
        /// The path the operation targeted.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// Hashing an input failed.
    #[error(transparent)]
    Hash(#[from] tinymold_hash::Error),

    /// No object with this digest is in the store.
    #[error("object {0} is not in the store")]
    NotFound(Digest),

    /// A stored object's bytes no longer hash to its name.
    #[error("object {expected} is corrupt: its contents hash to {actual}")]
    Corrupt {
        /// The digest the object is stored under.
        expected: Digest,
        /// The digest of the bytes actually on disk.
        actual: Digest,
    },

    /// Reading a stream passed to [`Store::stage`](crate::Store::stage)
    /// failed, such as a connection that dropped mid-upload.
    #[error("failed to read the stream being stored")]
    Stream(#[source] std::io::Error),

    /// The materialization destination already exists.
    ///
    /// The store never overwrites: replacing a file another process may have
    /// open or mapped is the caller's decision, not the store's.
    #[error("destination {0} already exists")]
    DestinationExists(PathBuf),

    /// A forced materialization strategy is not possible here, for example a
    /// reflink on a filesystem without copy-on-write support, or a hardlink
    /// across filesystems.
    #[error("cannot materialize {path} by {method}")]
    Unsupported {
        /// The method that was required.
        method: Method,
        /// The destination path.
        path: PathBuf,
        /// Why the platform refused it.
        #[source]
        source: std::io::Error,
    },
}

/// The crate's standard result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod test;
