//! Error and result types for the `tinymold` command line.
//!
//! Library errors from the hashing and store crates pass through unchanged,
//! so the binary can print their full cause chain.

use std::path::PathBuf;

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

    /// A cache record operation of the rustc wrapper failed.
    #[error(transparent)]
    Wrap(#[from] tinymold_wrap::Error),

    /// A duration such as `--max-age` is not a whole number followed by
    /// `s`, `m`, `h` or `d`.
    #[error("invalid duration {0:?}: use a whole number and s, m, h or d, such as 14d")]
    InvalidDuration(String),

    /// A size such as `--max-size` is not a whole number with an optional
    /// `K`, `M`, `G` or `T` suffix.
    #[error("invalid size {0:?}: use a whole number of bytes, or K, M, G or T, such as 10G")]
    InvalidSize(String),

    /// No store directory was given and none could be derived from the
    /// environment.
    #[error("no store location: pass --store or set TINYMOLD_STORE")]
    NoStoreLocation,

    /// Writing command output failed, for example because stdout was closed.
    #[error("failed to write output")]
    Output(#[source] std::io::Error),

    /// A filesystem operation on a workspace, target, or snapshot path failed.
    #[error("failed to {action} {path}")]
    Io {
        /// What was being attempted, such as `"read"` or `"clone"`.
        action: &'static str,
        /// The path the operation targeted.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// A freshness manifest could not be parsed.
    #[error("freshness manifest {path} is corrupt at line {line}")]
    CorruptManifest {
        /// The manifest file.
        path: PathBuf,
        /// The 1-based line that failed to parse.
        line: usize,
    },

    /// A path that must be a directory is not one.
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),

    /// A destination that must not exist already does.
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),

    /// A snapshot name contains characters other than ASCII letters, digits,
    /// `.`, `_` and `-`, or starts with `.`.
    #[error("invalid snapshot name {0:?}: use ASCII letters, digits, '.', '_' and '-'")]
    InvalidSnapshotName(String),

    /// No snapshot has the requested name.
    #[error("no snapshot named {0}")]
    SnapshotNotFound(String),

    /// A snapshot restore was asked to pick the best match, but the store
    /// holds no snapshots.
    #[error("the store holds no snapshots: run tinymold snapshot save after a build")]
    NoSnapshots,

    /// A restored snapshot's manifest does not describe its outputs, so its
    /// mtimes cannot be trusted; the restored copy was removed.
    #[error("snapshot {0} has no valid freshness manifest and was not restored")]
    SnapshotUnusable(String),

    /// No snapshot name was given and none could be derived from git.
    #[error("cannot name the snapshot from git HEAD: pass a name")]
    NoSnapshotName,
}

/// The crate's standard result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod test;
