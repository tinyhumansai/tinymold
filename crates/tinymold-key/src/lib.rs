//! Cache keys for tinymold.
//!
//! A cache hit is only safe if the key covers every input that can change a
//! rustc invocation's output, and only useful if it covers nothing else. This
//! crate turns one rustc command line into that key, in pure functions that
//! take file digests from the caller:
//!
//! - [`Invocation::parse`] classifies a command line as cacheable or not and
//!   picks out the crate name, outputs, externs and search paths;
//! - [`Roots`] rewrites absolute paths that differ between worktrees (the
//!   current directory, the out-dir, the profile directory, `CARGO_HOME`) to
//!   placeholders and back;
//! - [`DepInfo::parse`] reads which files and env vars a compile actually
//!   used;
//! - [`content_digest`] names a static archive by its members, not its
//!   build stamps;
//! - [`pre_key`] and [`full_key`] compute the two digests of the two-phase
//!   lookup described in `docs/specs/rustc-wrapper.md`.
//!
//! # Example
//!
//! ```
//! use tinymold_key::{Classified, Invocation};
//!
//! let args = ["--crate-name", "demo", "src/lib.rs", "--crate-type", "lib",
//!             "--emit=dep-info,metadata,link", "--out-dir", "/t/debug/deps"];
//! let Classified::Cacheable(invocation) = Invocation::parse(args.map(String::from)) else {
//!     panic!("a plain library compile is cacheable");
//! };
//! assert_eq!(invocation.crate_name, "demo");
//! assert!(!invocation.links());
//! ```
//!
//! # What this crate does not hold
//!
//! No I/O beyond what callers hand it, no processes, and no storage. Hashing
//! files, running rustc and keeping results belong to `tinymold-wrap`, so
//! that the rules deciding what a key covers can be read, and tested, on
//! their own.

mod archive;
mod args;
mod depinfo;
mod error;
mod key;
mod roots;

pub use archive::content_digest;
pub use args::{Classified, Extern, Invocation, SearchPath};
pub use depinfo::DepInfo;
pub use error::{Error, Result};
pub use key::{KEY_ENV, PreKeyInputs, full_key, pre_key};
pub use roots::Roots;
pub use tinymold_hash::Digest;
