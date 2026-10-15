//! tinymold's cache store: what is stored under a key, in a namespace.
//!
//! Every protocol tinymold's server speaks — REAPI, Bazel's HTTP cache,
//! Turborepo, Nx, Gradle — comes down to the same two questions: which
//! bytes are stored, and which result is stored under a client's key. This
//! crate answers both, once, for all of them
//! ([`docs/specs/cache-server.md`](https://github.com/tinyhumansai/tinymold/blob/main/docs/specs/cache-server.md)).
//!
//! - A [`Namespace`] is a front-end plus a tenant, and turns a client's key
//!   into a fixed-size [`Key`]. The same key bytes in two namespaces never
//!   meet.
//! - An [`Entry`] is what a key holds: the objects it needs, an optional
//!   body (a REAPI `ActionResult`), and a little metadata a front-end hands
//!   back on a read.
//! - [`Store::ingest`] streams a body into the content-addressed store and
//!   names it by both BLAKE3 and SHA-256 in one pass, refusing it when it
//!   does not match a digest the client claimed. [`Store::find_sha256`] maps
//!   a SHA-256 back to the stored object, so a blob that arrived through one
//!   protocol is found through another and kept once.
//!
//! # Example
//!
//! ```
//! use tinymold_store::{Entry, FrontEnd, Namespace, Store};
//!
//! let dir = tempfile::tempdir()?;
//! let store = Store::open(dir.path().join("store"))?;
//!
//! let artifact = store.ingest(&b"task output archive"[..], None)?;
//! let key = Namespace::new(FrontEnd::Turbo, "my-team")?.key(b"7c6a9e3f0b1d2e4a")?;
//! store.put(&key, &Entry::new(vec![artifact.digest]))?;
//!
//! let entry = store.get(&key)?.expect("just stored");
//! assert_eq!(store.cas().read(&entry.blobs[0])?, b"task output archive");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What this crate does not hold
//!
//! No network, no async runtime and no wire protocols: those are the
//! server's front-ends. No key computation either. Clients decide what a key
//! means; this crate only guarantees that a read returns exactly what was
//! stored under it.

mod entry;
mod error;
mod namespace;
mod sha256;
mod store;

pub use entry::{Entry, MAX_META};
pub use error::{Error, Result};
pub use namespace::{FrontEnd, Key, MAX_CLIENT_KEY, MAX_TENANT, Namespace};
pub use sha256::Sha256;
pub use store::{Claim, Ingested, Store};
pub use tinymold_hash::Digest;
