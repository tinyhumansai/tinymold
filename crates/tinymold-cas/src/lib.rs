//! tinymold's local content-addressed store.
//!
//! A [`Store`] keeps build artifacts — rlibs, rmeta files, binaries, anything
//! a build produces — as immutable objects named by their
//! [`Digest`]. Identical bytes are stored once, no
//! matter how many worktrees, agents, or projects produce them, which is how
//! tinymold keeps N checkouts of one project from costing N copies of its
//! dependency artifacts.
//!
//! Getting bytes back out is a [`Store::materialize`] call that places an
//! object at a destination path, preferring a copy-on-write clone, then a
//! hardlink, then a plain copy (see [`Strategy`]). The first two cost no
//! data I/O at all, which is what makes a cache hit cheaper than a rebuild
//! even for multi-hundred-megabyte artifacts.
//!
//! # Example
//!
//! ```
//! use tinymold_cas::{Store, Strategy};
//!
//! let dir = tempfile::tempdir()?;
//! let store = Store::open(dir.path().join("store"))?;
//!
//! let digest = store.insert_bytes(b"lib.rmeta bytes")?;
//! assert!(store.contains(&digest));
//!
//! let restored = dir.path().join("target/libdemo.rmeta");
//! store.materialize(&digest, &restored, Strategy::Auto)?;
//! assert_eq!(std::fs::read(&restored)?, b"lib.rmeta bytes");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What this crate does not hold
//!
//! No cache keys, no eviction, and no network. Which digest answers which
//! build step is the action cache's job, deciding what to delete is the hot
//! server's, and remote tiers live in their own crate. Keeping this layer to
//! "bytes in, bytes out, by name" is what lets every other layer trust it.

mod error;
mod materialize;
mod store;

pub use error::{Error, Result};
pub use materialize::{Method, Strategy};
pub use store::Store;
pub use tinymold_hash::Digest;
