//! `tinymold serve`: running the cache server from the command line.
//!
//! This module only turns arguments into a [`tinymold_server::Config`],
//! guards against serving the wrapper's store, and runs the server until
//! Ctrl-C or `SIGTERM`. The server itself is `tinymold-server`.

use std::io::Write;
#[cfg(feature = "server")]
use std::path::PathBuf;

use crate::cli::{Cli, Outcome, ServeArgs};
use crate::{Error, Result};

/// Runs the server described by `args` until the process is asked to stop.
///
/// # Errors
///
/// Returns [`Error::NoServerDir`] when no directory can be found,
/// [`Error::SharedStoreDir`] when it is the wrapper's store,
/// [`Error::Server`] when the server cannot start, and [`Error::NoServer`]
/// on a binary built without the `server` feature.
#[cfg(feature = "server")]
pub(crate) fn run(cli: &Cli, args: &ServeArgs, out: &mut impl Write) -> Result<Outcome> {
    run_until(cli, args, out, tinymold_server::shutdown_signal())
}

/// Runs the server described by `args` until `shutdown` completes.
#[cfg(feature = "server")]
fn run_until(
    cli: &Cli,
    args: &ServeArgs,
    out: &mut impl Write,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<Outcome> {
    start(cli, args, out)?.run_blocking(shutdown)?;
    Ok(Outcome::Success)
}

/// Binds the server `args` describe and says where it listens, without
/// serving yet.
#[cfg(feature = "server")]
fn start(cli: &Cli, args: &ServeArgs, out: &mut impl Write) -> Result<tinymold_server::Server> {
    let dir = args
        .dir
        .clone()
        .or_else(|| crate::default_server_dir(|name| std::env::var_os(name)))
        .ok_or(Error::NoServerDir)?;
    if let Some(wrapper_store) = crate::cli::store_dir(cli) {
        crate::location::refuse_shared(&dir, &wrapper_store)?;
    }
    let config = config(args, dir);
    let server = tinymold_server::Server::bind(&config)?;
    crate::cli::line(
        out,
        format_args!(
            "tinymold serve: listening on {}://{} (store {})",
            scheme(&config),
            server.local_addr(),
            config.store.display()
        ),
    )?;
    let _ = out.flush();
    Ok(server)
}

/// The URL scheme clients reach `config` at.
#[cfg(feature = "server")]
fn scheme(config: &tinymold_server::Config) -> &'static str {
    if config.tls.is_some() {
        "https"
    } else {
        "http"
    }
}

/// Without the server feature there is nothing to run.
#[cfg(not(feature = "server"))]
pub(crate) fn run(_cli: &Cli, _args: &ServeArgs, _out: &mut impl Write) -> Result<Outcome> {
    Err(Error::NoServer)
}

/// The server configuration `args` describe, serving `dir`.
#[cfg(feature = "server")]
pub(crate) fn config(args: &ServeArgs, dir: PathBuf) -> tinymold_server::Config {
    let mut config = tinymold_server::Config::new(dir);
    config.listen = args.listen;
    config.tokens.clone_from(&args.tokens);
    config.allow_anonymous_reads = args.allow_anonymous_reads;
    if let (Some(cert), Some(key)) = (&args.tls_cert, &args.tls_key) {
        config.tls = Some(tinymold_server::TlsFiles {
            cert: cert.clone(),
            key: key.clone(),
        });
    }
    config.max_size = args.max_size.map(|size| size.0);
    config.max_age = args.max_age.map(|age| age.0);
    if let Some(size) = args.max_body {
        config.max_body = size.0;
    }
    if let Some(age) = args.gc_interval {
        config.gc_interval = age.0;
    }
    config
}

#[cfg(all(test, feature = "server"))]
mod test;
