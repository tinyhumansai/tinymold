//! tinymold's remote cache tier: unit-cache entries shared between machines
//! through any REAPI v2 cache, such as bazel-remote.
//!
//! A fresh CI runner starts with an empty store. With a remote configured,
//! the wrapper asks the remote on every local miss before compiling, and a
//! job ends with [`RemoteCache::push`], which uploads what it built in one
//! batch. The design is in `docs/specs/remote.md` and
//! `docs/adr/0006-remote-tier-reapi-bazel-remote.md`.
//!
//! - [`RemoteConfig`] reads `TINYMOLD_REMOTE_URL`, `TINYMOLD_REMOTE_AUTH`,
//!   `TINYMOLD_REMOTE_INSTANCE` and `TINYMOLD_REMOTE_TIMEOUT`;
//! - [`RemoteCache`] implements [`tinymold_wrap::Remote`] for the wrapper,
//!   plus [`RemoteCache::push`] and [`RemoteCache::check`].
//!
//! # Example
//!
//! ```no_run
//! use tinymold_remote::{RemoteCache, RemoteConfig};
//!
//! let config = RemoteConfig::from_env(|name| std::env::var_os(name))?
//!     .expect("TINYMOLD_REMOTE_URL is set");
//! let remote = RemoteCache::new(&config)?;
//! println!("{}", remote.check()?);
//! let report = remote.push("/home/me/.cache/tinymold".as_ref(), None)?;
//! println!("{report}");
//! # Ok::<(), tinymold_remote::Error>(())
//! ```
//!
//! # What this crate does not hold
//!
//! No cache keys and no restore logic: which entry answers a compile, and
//! whether the objects a remote sent may be trusted, is decided by
//! `tinymold-wrap`, which checks every object against the record it parsed
//! itself. No server either: tinymold talks to existing REAPI caches. This is
//! the only crate with an async runtime, so the wrapper's hot path stays
//! synchronous when no remote is configured.

mod client;
mod config;
mod error;
mod naming;
mod proto;
mod remote;

pub use config::{DEFAULT_TIMEOUT, RemoteConfig};
pub use error::{Error, Result};
pub use remote::{Capabilities, PushReport, RemoteCache};
