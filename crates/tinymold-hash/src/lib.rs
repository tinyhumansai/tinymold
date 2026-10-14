//! Content digests for tinymold.
//!
//! Every byte tinymold caches, shares, or compares is named by a [`Digest`]: the
//! 32-byte BLAKE3 hash of its contents. Cache keys, store objects, and the
//! output manifests that tie them together all speak in digests, so two
//! machines, two worktrees, or two agents that hold the same bytes agree on
//! their name without talking to each other.
//!
//! BLAKE3 was chosen because a shared cache needs a collision-resistant hash,
//! and BLAKE3 is the fastest one available on both x86 and Apple Silicon. The
//! reasoning is recorded in `docs/adr/0002-blake3-for-content-digests.md`.
//!
//! # Example
//!
//! ```
//! use tinymold_hash::Digest;
//!
//! let digest = Digest::of_bytes(b"fn main() {}");
//! let hex = digest.to_hex();
//! assert_eq!(hex.len(), 64);
//! assert_eq!(hex.parse::<Digest>()?, digest);
//! # Ok::<(), tinymold_hash::Error>(())
//! ```
//!
//! # What this crate does not hold
//!
//! It holds no cache policy and no storage. Deciding *what* to hash belongs to
//! the cache-key layer, and keeping hashed bytes belongs to `tinymold-cas`.
//! This crate only answers "what is the name of these bytes".

mod digest;
mod error;

pub use digest::{Digest, Hasher};
pub use error::{Error, Result};
