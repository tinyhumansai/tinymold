//! tinymold: make Rust builds fast and small.
//!
//! tinymold attacks the four costs that dominate a Rust edit-build-test loop
//! and the disk it leaves behind: recompiling what was already compiled
//! somewhere (**caching**), relinking every binary from scratch
//! (**linking**), cold processes that rediscover state on every invocation
//! (the **hot cache server**), and reading and rehashing unchanged files
//! (**hashing**). The design is in `docs/specs/architecture.md` and the
//! phased plan in `docs/plans/roadmap.md`.
//!
//! This crate is the `tinymold` binary and its library half. Today it exposes
//! the foundation those layers build on:
//!
//! - `tinymold hash` — BLAKE3 digests of files, via [`tinymold_hash`];
//! - `tinymold cas` — the local content-addressed store, via [`tinymold_cas`];
//! - `tinymold doctor` — which fast linkers and caches this machine has;
//! - `tinymold fresh` — undo mtime changes that did not change content, via
//!   [`fresh`];
//! - `tinymold snapshot` — whole `target/` snapshots for new worktrees and
//!   branch switches, via [`snapshot`];
//! - `tinymold gc` — remove cache entries unused for a while, or beyond a
//!   size budget, via [`gc`];
//! - `tinymold stats` — hit rates and times from the wrapper's log, via
//!   [`stats`];
//! - `RUSTC_WRAPPER=tinymold` — a unit cache keyed by content, via
//!   [`tinymold_wrap`] (see [`wrapper_call`]);
//! - `tinymold remote` — share that cache between machines through a REAPI
//!   server, via `tinymold_remote` (the default `remote` feature; see
//!   [`wrapper_remote`]).
//!
//! # Example
//!
//! ```
//! use clap::Parser;
//! use tinymold::{Cli, Outcome};
//!
//! let dir = tempfile::tempdir()?;
//! let file = dir.path().join("lib.rs");
//! std::fs::write(&file, "pub fn answer() -> u8 { 42 }")?;
//!
//! let cli = Cli::try_parse_from(["tinymold", "hash", file.to_str().unwrap()])?;
//! let mut out = Vec::new();
//! assert_eq!(tinymold::run(&cli, &mut out)?, Outcome::Success);
//! assert!(String::from_utf8(out)?.ends_with("lib.rs\n"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod cli;
mod doctor;
mod error;
pub mod fresh;
pub mod gc;
mod location;
pub mod snapshot;
pub mod stats;
mod tree;
mod wrapper;

pub use cli::{
    CasCommand, Cli, Command, FreshCommand, GcArgs, Location, Outcome, RemoteCommand, RunTestArgs,
    SnapshotCommand, StatsArgs, StrategyArg, run,
};
pub use doctor::{Report, Tool, find_on_path};
pub use error::{Error, Result};
pub use location::default_store;
pub use tinymold_cas;
pub use tinymold_hash;
#[cfg(feature = "remote")]
pub use tinymold_remote;
pub use tinymold_wrap;
pub use tree::{TreeClone, clone_tree};
pub use wrapper::{wrapper_call, wrapper_config, wrapper_remote};
