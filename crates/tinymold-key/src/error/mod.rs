//! Error and result types for cache-key computation.

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A dep-info file could not be parsed.
    #[error("malformed dep-info at line {line}")]
    MalformedDepInfo {
        /// The 1-based line that failed to parse.
        line: usize,
    },
}

/// The crate's standard result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod test;
