//! Error and result types for the `tinymold` command line.
//!
//! Library errors from the hashing and store crates pass through unchanged,
//! so the binary can print their full cause chain.

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Hashing an input failed.
    #[error(transparent)]
    Hash(#[from] tinymold_hash::Error),

    /// A content-addressed store operation failed.
    #[error(transparent)]
    Cas(#[from] tinymold_cas::Error),

    /// No store directory was given and none could be derived from the
    /// environment.
    #[error("no store location: pass --store or set TINYMOLD_STORE")]
    NoStoreLocation,

    /// Writing command output failed, for example because stdout was closed.
    #[error("failed to write output")]
    Output(#[source] std::io::Error),
}

/// The crate's standard result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod test;
