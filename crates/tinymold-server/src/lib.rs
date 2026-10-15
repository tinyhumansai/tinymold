//! `tinymold serve`: one content-addressed build cache behind many
//! remote-cache protocols.
//!
//! A [`Server`] listens on one address and serves every front-end from one
//! [`tinymold_store::Store`], so identical bytes are stored once whichever
//! protocol they arrive through ([`docs/specs/cache-server.md`]). It speaks
//! HTTP/1.1 and HTTP/2, plaintext or TLS, authenticates requests against a
//! token file ([`Tokens`]), exposes `/healthz` and Prometheus `/metrics`, and
//! collects garbage in the background.
//!
//! # Example
//!
//! ```no_run
//! use tinymold_server::{Config, Server};
//!
//! let mut config = Config::new("/var/cache/tinymold-server");
//! config.max_size = Some(50 << 30);
//! let server = Server::bind(&config)?;
//! println!("listening on {}", server.local_addr());
//! // Serve until Ctrl-C or SIGTERM.
//! server.run_blocking(tinymold_server::shutdown_signal())?;
//! # Ok::<(), tinymold_server::Error>(())
//! ```
//!
//! # What this crate does not hold
//!
//! No storage logic and no key computation: entries, digests and eviction
//! are `tinymold-store`'s, and keys are the clients'. The server can tell
//! that a read returns what was stored, never that a hit is correct.
//!
//! [`docs/specs/cache-server.md`]: https://github.com/tinyhumansai/tinymold/blob/main/docs/specs/cache-server.md

mod auth;
mod config;
mod error;
mod front;
mod metrics;
mod route;
mod server;
mod transfer;

pub use auth::{Access, Denied, Permission, Tokens};
pub use config::{Config, DEFAULT_GC_INTERVAL, DEFAULT_LISTEN, DEFAULT_MAX_BODY, TlsFiles};
pub use error::{Error, Result};
pub use server::{Server, shutdown_signal};
