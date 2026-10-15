//! What ingesting a body takes and reports.

use std::fmt;

use tinymold_hash::Digest;

use crate::Sha256;

/// A digest a client says its body has, checked before the body is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// A BLAKE3 digest, as tinymold's own clients send.
    Blake3(Digest),
    /// A SHA-256 digest, as REAPI and Bazel HTTP clients send.
    Sha256(Sha256),
}

impl fmt::Display for Claim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blake3(digest) => write!(f, "blake3:{digest}"),
            Self::Sha256(digest) => write!(f, "sha256:{digest}"),
        }
    }
}

/// A body the store now holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ingested {
    /// Its BLAKE3 digest: the object's name in the store.
    pub digest: Digest,
    /// Its SHA-256 digest, under which the index finds it.
    pub sha256: Sha256,
    /// Its size in bytes.
    pub size: u64,
}
