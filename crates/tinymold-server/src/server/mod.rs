//! The listener: accepting connections and serving them until shutdown.
//!
//! One listener speaks HTTP/1.1 and HTTP/2, with or without TLS, through
//! hyper-util's auto builder. Plaintext HTTP/2 (h2c, which gRPC clients use
//! on `grpc://` URLs) is recognised by its connection preface. A background
//! task collects garbage on [`Config::gc_interval`].

use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use tinymold_store::{Policy, Store, collect};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls;

use crate::auth::{Access, Tokens};
use crate::config::{Config, TlsFiles};
use crate::metrics::Metrics;
use crate::route::{self, State};
use crate::{Error, Result};

/// How long open connections get to finish after shutdown is asked for.
const DRAIN: Duration = Duration::from_secs(10);

/// A bound, not yet running, cache server.
pub struct Server {
    listener: std::net::TcpListener,
    local_addr: SocketAddr,
    state: Arc<State>,
    tls: Option<TlsAcceptor>,
    policy: Policy,
    gc_interval: Duration,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("local_addr", &self.local_addr)
            .field("tls", &self.tls.is_some())
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Opens the store, reads the token and TLS files, and binds the
    /// listen address.
    ///
    /// # Errors
    ///
    /// Returns [`Error::OpenNonLoopback`] for a non-loopback address
    /// without a token file, [`Error::ReadFile`], [`Error::TokenFile`],
    /// [`Error::TlsPem`] or [`Error::Tls`] for bad configuration files,
    /// [`Error::Store`] when the store cannot be opened, and
    /// [`Error::Bind`] when the address cannot be bound.
    pub fn bind(config: &Config) -> Result<Self> {
        let access = match &config.tokens {
            Some(path) => Access::new(Tokens::load(path)?, config.allow_anonymous_reads),
            None if config.listen.ip().is_loopback() => Access::open(),
            None => return Err(Error::OpenNonLoopback(config.listen)),
        };
        let tls = config.tls.as_ref().map(tls_acceptor).transpose()?;
        let store = Store::open(&config.store)?;

        let bind_error = |source| Error::Bind {
            addr: config.listen,
            source,
        };
        let listener = std::net::TcpListener::bind(config.listen).map_err(bind_error)?;
        listener.set_nonblocking(true).map_err(bind_error)?;
        let local_addr = listener.local_addr().map_err(bind_error)?;

        Ok(Self {
            listener,
            local_addr,
            state: Arc::new(State {
                store,
                access,
                metrics: Metrics::default(),
                max_body: config.max_body,
            }),
            tls,
            policy: Policy {
                max_age: config.max_age,
                max_size: config.max_size,
                ..Policy::garbage_only()
            },
            gc_interval: config.gc_interval,
        })
    }

    /// The address the server is listening on, with the port the system
    /// chose when the configuration asked for port 0.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Serves until `shutdown` completes, then stops accepting, gives open
    /// connections a few seconds to finish, and collects garbage one last
    /// time with no grace period, so the store on disk fits its budget.
    ///
    /// Must be called inside a multi-threaded tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runtime`] when the listener cannot be registered
    /// with the runtime.
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let listener = TcpListener::from_std(self.listener).map_err(Error::Runtime)?;
        let graceful = GracefulShutdown::new();
        let builder = auto::Builder::new(TokioExecutor::new());
        let collector = tokio::spawn(collect_periodically(
            self.state.clone(),
            self.policy,
            self.gc_interval,
        ));

        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    // A failed accept (too many open files, a reset before
                    // accept) concerns one client; keep serving the rest.
                    let Ok((stream, _peer)) = accepted else { continue };
                    let _ = stream.set_nodelay(true);
                    let state = self.state.clone();
                    let service = service_fn(move |request| route::handle(state.clone(), request));
                    let builder = builder.clone();
                    let watcher = graceful.watcher();
                    match self.tls.clone() {
                        None => {
                            tokio::spawn(async move {
                                let connection = builder.serve_connection(TokioIo::new(stream), service);
                                let _ = watcher.watch(connection.into_owned()).await;
                            });
                        }
                        Some(acceptor) => {
                            tokio::spawn(async move {
                                let Ok(stream) = acceptor.accept(stream).await else { return };
                                let connection = builder.serve_connection(TokioIo::new(stream), service);
                                let _ = watcher.watch(connection.into_owned()).await;
                            });
                        }
                    }
                }
                () = &mut shutdown => break,
            }
        }

        drop(listener);
        collector.abort();
        let _ = tokio::time::timeout(DRAIN, graceful.shutdown()).await;

        // One last collection to the budget. Nothing is uploading any more,
        // so no grace period is needed: whatever no entry keeps can go now,
        // before a CI job saves the store.
        let state = self.state.clone();
        let policy = Policy {
            grace: Duration::ZERO,
            now: std::time::SystemTime::now(),
            ..self.policy
        };
        match tokio::task::spawn_blocking(move || collect(&state.store, &policy)).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => eprintln!("tinymold serve: final collection failed: {error}"),
            Err(_) => eprintln!("tinymold serve: final collection stopped unexpectedly"),
        }
        Ok(())
    }

    /// Starts a multi-threaded runtime and serves on it until `shutdown`
    /// completes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runtime`] when the runtime cannot be started, and
    /// whatever [`Server::run`] returns.
    pub fn run_blocking(self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(Error::Runtime)?;
        runtime.block_on(self.run(shutdown))
    }
}

/// Completes when the process is asked to stop: Ctrl-C, or `SIGTERM` on
/// Unix, as a service manager or Kubernetes sends.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Collects garbage now and then every `interval`, recording the store's
/// size in the metrics. A failed collection is reported and retried on the
/// next tick.
async fn collect_periodically(state: Arc<State>, policy: Policy, interval: Duration) {
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let worker = state.clone();
        let policy = Policy {
            now: std::time::SystemTime::now(),
            ..policy
        };
        match tokio::task::spawn_blocking(move || collect(&worker.store, &policy)).await {
            Ok(Ok(report)) => state.metrics.record_collection(&report),
            Ok(Err(error)) => eprintln!("tinymold serve: collection failed: {error}"),
            Err(_) => eprintln!("tinymold serve: collection stopped unexpectedly"),
        }
    }
}

/// Builds the TLS acceptor from PEM files, offering HTTP/2 and HTTP/1.1.
fn tls_acceptor(files: &TlsFiles) -> Result<TlsAcceptor> {
    use rustls_pki_types::pem::PemObject as _;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};

    let cert_pem = read(&files.cert)?;
    let certs = CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| pem_error(&files.cert, "not a pem certificate file"))?;
    if certs.is_empty() {
        return Err(pem_error(&files.cert, "no certificate in file"));
    }
    let key_pem = read(&files.key)?;
    let key = PrivateKeyDer::from_pem_slice(&key_pem)
        .map_err(|_| pem_error(&files.key, "no private key in file"))?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(Error::Tls)?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(Error::Tls)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| Error::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

fn pem_error(path: &Path, reason: &'static str) -> Error {
    Error::TlsPem {
        path: path.to_path_buf(),
        reason,
    }
}
