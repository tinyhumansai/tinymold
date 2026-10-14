//! Error and result types for content hashing.
//!
//! Every fallible public function in this crate returns [`Result`], and every
//! failure mode is a distinct [`Error`] variant that callers can match on.

use std::path::PathBuf;

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Reading the bytes to hash failed.
    #[error("failed to read {path}")]
    Read {
        /// The file being hashed, or `<reader>` for an anonymous stream.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// A digest's text form was not exactly 64 hexadecimal characters.
    #[error("invalid digest {text:?}: expected 64 hexadecimal characters")]
    InvalidHex {
        /// The rejected text.
        text: String,
    },
}

/// The crate's standard result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod test;
