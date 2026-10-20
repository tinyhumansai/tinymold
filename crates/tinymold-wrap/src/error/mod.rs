//! Error and result types for the rustc wrapper.
//!
//! None of these reach cargo as a failed build: the wrapper falls back to
//! running rustc whenever the cache fails. They exist so that the fallback can
//! say why, in remarks and in the log.

use std::path::PathBuf;

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation failed.
    #[error("failed to {action} {path}")]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// The path the operation targeted.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// Hashing a file failed.
    #[error(transparent)]
    Hash(#[from] tinymold_hash::Error),

    /// A store operation failed.
    #[error(transparent)]
    Cas(#[from] tinymold_cas::Error),

    /// Computing a key failed.
    #[error(transparent)]
    Key(#[from] tinymold_key::Error),

    /// A cache record could not be parsed.
    #[error("cache record {path} is corrupt at line {line}")]
    CorruptRecord {
        /// The record file.
        path: PathBuf,
        /// The 1-based line that failed to parse.
        line: usize,
    },

    /// An action names an output that does not belong to this invocation.
    #[error("cached output {0} does not belong to this invocation")]
    UnexpectedOutput(String),

    /// A program could not be started.
    #[error("failed to run {program}")]
    Spawn {
        /// The program.
        program: PathBuf,
        /// Why it could not start.
        #[source]
        source: std::io::Error,
    },

    /// A worker thread panicked partway through a job split across threads.
    #[error("a worker thread panicked while {task}")]
    WorkerPanicked {
        /// What the threads were doing.
        task: &'static str,
    },
}

/// The crate's standard result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Builds the error-mapping closure for an I/O failure on `path`.
pub(crate) fn io(
    action: &'static str,
    path: &std::path::Path,
) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io {
        action,
        path,
        source,
    }
}

#[cfg(test)]
mod test;
