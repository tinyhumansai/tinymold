//! Error and result types for the server.
//!
//! Errors here are about starting and running the server: binding, reading
//! configuration, TLS. A failed request is answered with the protocol's own
//! status and never surfaces as an [`Error`].

use std::net::SocketAddr;
use std::path::PathBuf;

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The cache store failed to open or collect.
    #[error(transparent)]
    Store(#[from] tinymold_store::Error),

    /// The listen address could not be bound.
    #[error("failed to listen on {addr}")]
    Bind {
        /// The address that was asked for.
        addr: SocketAddr,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// A non-loopback address was asked for without a token file, which
    /// would let anyone on the network read and write the cache.
    #[error("refusing to serve {0} without a token file: only loopback addresses may be open")]
    OpenNonLoopback(SocketAddr),

    /// A configuration file could not be read.
    #[error("failed to read {path}")]
    ReadFile {
        /// The file.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// A line of the token file is not `<token> <scope>...`.
    #[error("token file line {line}: {reason}")]
    TokenFile {
        /// The 1-based line number.
        line: usize,
        /// What is wrong with it.
        reason: &'static str,
    },

    /// A TLS certificate or key file holds no usable PEM item.
    #[error("{path}: {reason}")]
    TlsPem {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        reason: &'static str,
    },

    /// The certificate and key do not make a valid TLS configuration.
    #[error("invalid tls configuration")]
    Tls(#[source] tokio_rustls::rustls::Error),

    /// The async runtime could not be started, or the listener could not be
    /// handed to it.
    #[error("failed to start the server runtime")]
    Runtime(#[source] std::io::Error),
}

/// The crate's standard result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod test;
