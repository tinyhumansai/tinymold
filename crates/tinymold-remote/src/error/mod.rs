//! Error and result types for the remote tier.
//!
//! The wrapper turns every one of these into a miss, so a build never fails
//! because the remote did. `tinymold remote push` and `check` report them.

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `TINYMOLD_REMOTE_URL` is not a `grpc://`, `grpcs://`, `http://` or
    /// `https://` URL with a host.
    #[error("invalid remote URL {0:?}: use grpc://host:port or grpcs://host[:port]")]
    InvalidUrl(String),

    /// `TINYMOLD_REMOTE_AUTH` is not `user:password`.
    #[error("invalid remote credential: use user:password")]
    InvalidAuth,

    /// `TINYMOLD_REMOTE_TIMEOUT` is not a whole number of `s` or `ms`.
    #[error("invalid remote timeout {0:?}: use whole seconds or milliseconds, such as 5s")]
    InvalidTimeout(String),

    /// The connection to the server could not be set up.
    #[error("cannot connect to the remote cache")]
    Connect(#[source] tonic::transport::Error),

    /// The async runtime the client runs on could not start.
    #[error("cannot start the remote client's runtime")]
    Runtime(#[source] std::io::Error),

    /// The server answered a call with an error status.
    #[error("the remote cache refused {call}: {status}")]
    Status {
        /// The call, such as `GetActionResult`.
        call: &'static str,
        /// What the server said.
        #[source]
        status: Box<tonic::Status>,
    },

    /// A call took longer than the configured timeout.
    #[error("the remote cache did not answer {0} in time")]
    Timeout(&'static str),

    /// A blob the server sent does not hash to the digest it was asked for.
    #[error("the remote cache sent blob {expected} with the wrong content")]
    BlobMismatch {
        /// The SHA-256 the client asked for.
        expected: String,
    },

    /// A blob's declared size is negative or over the limit, or the server
    /// sent more bytes than it declared.
    #[error("the remote cache declared blob {hash} as {size} bytes, which tinymold will not read")]
    BlobSize {
        /// The SHA-256 the client asked for.
        hash: String,
        /// The size the digest declared.
        size: i64,
    },

    /// An entry the server holds is not one tinymold wrote.
    #[error("remote entry {key} is not a tinymold entry: {reason}")]
    Malformed {
        /// The `ActionCache` key.
        key: String,
        /// What is wrong with it.
        reason: &'static str,
    },

    /// The local store failed.
    #[error(transparent)]
    Cas(#[from] tinymold_cas::Error),

    /// Listing or reading the local cache records failed.
    #[error(transparent)]
    Wrap(#[from] tinymold_wrap::Error),
}

/// The crate's standard result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod test;
