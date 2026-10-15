//! What `tinymold serve` is started with.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// The default listen address: loopback only, so a server started without a
/// token file is reachable from this machine alone.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:9090";

/// The largest request body accepted by default: 4 GiB, more than any build
/// artifact a cache should hold.
pub const DEFAULT_MAX_BODY: u64 = 4 << 30;

/// How long an entry no client reads or writes is kept by default: two
/// days, as `docs/specs/cache-server.md` sets for `--max-age`.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(48 * 3600);

/// How often the server collects garbage by default.
pub const DEFAULT_GC_INTERVAL: Duration = Duration::from_secs(600);

/// A certificate chain and private key, both PEM files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    /// The certificate chain, leaf first.
    pub cert: PathBuf,
    /// The private key for the leaf certificate.
    pub key: PathBuf,
}

/// How to run the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The address to listen on.
    pub listen: SocketAddr,
    /// The store directory. Never the wrapper's local store.
    pub store: PathBuf,
    /// The token file; `None` allows every request and requires a loopback
    /// `listen` address.
    pub tokens: Option<PathBuf>,
    /// Let requests without a credential read.
    pub allow_anonymous_reads: bool,
    /// Serve TLS with these files instead of plaintext.
    pub tls: Option<TlsFiles>,
    /// Evict least recently used entries beyond this many object bytes.
    pub max_size: Option<u64>,
    /// Remove entries unused for this long; `None` keeps them whatever
    /// their age.
    pub max_age: Option<Duration>,
    /// How often to collect garbage.
    pub gc_interval: Duration,
    /// The largest request body accepted, in bytes. Larger uploads are
    /// refused with the protocol's "too large" status before they are read.
    pub max_body: u64,
}

impl Config {
    /// A configuration serving `store` on [`DEFAULT_LISTEN`], open, without
    /// TLS or a size budget, expiring entries after [`DEFAULT_MAX_AGE`].
    #[must_use]
    pub fn new(store: impl Into<PathBuf>) -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 9090)),
            store: store.into(),
            tokens: None,
            allow_anonymous_reads: false,
            tls: None,
            max_size: None,
            max_age: Some(DEFAULT_MAX_AGE),
            gc_interval: DEFAULT_GC_INTERVAL,
            max_body: DEFAULT_MAX_BODY,
        }
    }
}
