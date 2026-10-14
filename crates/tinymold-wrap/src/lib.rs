//! tinymold's `RUSTC_WRAPPER`: a unit cache keyed by content.
//!
//! Cargo runs `tinymold <rustc> <args…>` for every compile. [`run`] keys a
//! cacheable compile by everything that can change its output (see
//! `docs/specs/rustc-wrapper.md`), restores the outputs on a hit without
//! running rustc, and on a miss runs rustc with its output streamed through
//! and stores what it produced. Anything it cannot key, it runs unchanged.
//!
//! The cache lives in the same store as `tinymold cas`: objects under
//! `objects/`, actions under `entries/<crate>/`, input sets under
//! `prekeys/`, and the digest cache under `stat/`. [`records`] lists them,
//! with each one's last use, for `tinymold gc`.
//!
//! # Example
//!
//! ```no_run
//! use std::ffi::OsString;
//! use std::path::Path;
//!
//! let config = tinymold_wrap::Config::new("/home/me/.cache/tinymold");
//! let args: Vec<OsString> = std::env::args_os().skip(2).collect();
//! let code = tinymold_wrap::run(&config, Path::new("rustc"), &args);
//! std::process::exit(code);
//! ```
//!
//! # What this crate does not hold
//!
//! The rules for what a key covers live in `tinymold-key`, and bytes live in
//! `tinymold-cas`. This crate is the process glue between them and rustc:
//! hashing files, running rustc, and moving outputs in and out of the store.

mod catalog;
mod clock;
mod compile;
mod error;
mod exec;
mod parallel;
mod record;
mod remote;
mod stat;
mod trace;

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tinymold_key::{Classified, Invocation};

pub use catalog::{RecordFile, RecordKind, action_references, records};
pub use clock::{filesystem_now, written_before};
use compile::Served;
pub use error::{Error, Result};
pub use remote::{Remote, RemoteError};
use trace::Trace;

/// Where the wrapper keeps its cache and reports what it did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
    /// The store directory.
    pub store: PathBuf,
    /// A file to append one line per invocation to (`hit`, `remote`, `miss`,
    /// `pass` or `error`, the crate, milliseconds, and details), for
    /// benchmarks, `tinymold stats` and debugging.
    pub log: Option<PathBuf>,
    /// A file to append one Chrome trace event per wrapper step to (hashing,
    /// lookup, restore, rustc, storing), for profiling where a build's time
    /// goes. See `bench/profile_build.py`.
    pub trace: Option<PathBuf>,
}

impl Config {
    /// A configuration using the store at `store`, without a log.
    #[must_use]
    pub fn new(store: impl Into<PathBuf>) -> Self {
        Self {
            store: store.into(),
            log: None,
            trace: None,
        }
    }
}

/// Whether `program` is a compiler the wrapper may cache: a file named
/// `rustc`. Other wrapped programs, such as `clippy-driver`, run unchanged.
#[must_use]
pub fn is_rustc(program: &Path) -> bool {
    program.file_stem().is_some_and(|stem| stem == "rustc")
}

/// Runs one wrapped compiler invocation and returns the exit code cargo
/// should see.
///
/// Cache failures never fail the build: the wrapper then runs the compiler
/// as cargo asked. Only a compiler that cannot be started at all is reported,
/// with exit code 1.
#[must_use]
pub fn run(config: &Config, program: &Path, args: &[OsString]) -> i32 {
    run_with_remote(config, None, program, args)
}

/// [`run`], consulting `remote` on a local miss before compiling.
///
/// A remote failure is logged with the miss and never fails the build.
#[must_use]
pub fn run_with_remote(
    config: &Config,
    remote: Option<&dyn Remote>,
    program: &Path,
    args: &[OsString],
) -> i32 {
    let started = Instant::now();
    let mut trace = Trace::start();
    let (outcome, crate_name, code) = serve(config, remote, program, args, started, &mut trace);
    if let Some(path) = &config.trace {
        trace.write(path, &crate_name, outcome);
    }
    code
}

/// Serves one invocation and returns how (`hit`, `remote`, `miss`, `pass` or
/// `error`), the crate it compiled, and the exit code.
fn serve(
    config: &Config,
    remote: Option<&dyn Remote>,
    program: &Path,
    args: &[OsString],
    started: Instant,
    trace: &mut Trace,
) -> (&'static str, String, i32) {
    let classified = trace.span("classify", || match utf8(args) {
        Some(text) if is_rustc(program) => Invocation::parse(text),
        Some(_) => Classified::PassThrough("not rustc"),
        None => Classified::PassThrough("an argument is not UTF-8"),
    });
    let invocation = match classified {
        Classified::Cacheable(invocation) => invocation,
        Classified::PassThrough(reason) => {
            log(config, started, "pass", "-", reason);
            let code = trace.span("pass_through", || pass_through(program, args));
            return ("pass", "-".to_owned(), code);
        }
        _ => {
            let code = trace.span("pass_through", || pass_through(program, args));
            return ("pass", "-".to_owned(), code);
        }
    };
    let crate_name = invocation.crate_name.clone();
    match compile::serve(&config.store, remote, program, args, &invocation, trace) {
        Ok(Served::Hit) => {
            log(config, started, "hit", &crate_name, "");
            ("hit", crate_name, 0)
        }
        Ok(Served::RemoteHit) => {
            log(config, started, "remote", &crate_name, "");
            ("remote", crate_name, 0)
        }
        Ok(Served::Miss {
            code,
            stored,
            remote,
        }) => {
            let mut detail = match &stored {
                Ok(took) => format!("stored in {:.1}ms", took.as_secs_f64() * 1000.0),
                Err(reason) => format!("not stored: {reason}"),
            };
            if let Some(failure) = remote {
                detail.push_str("; remote: ");
                detail.push_str(&failure);
            }
            log(config, started, "miss", &crate_name, &detail);
            ("miss", crate_name, code)
        }
        Err(error) => {
            log(config, started, "error", &crate_name, &error.to_string());
            let code = trace.span("pass_through", || pass_through(program, args));
            ("error", crate_name, code)
        }
    }
}

/// Runs the compiler unchanged and returns its exit code, or 1 when it cannot
/// be started. Used when no store is available.
#[must_use]
pub fn run_uncached(program: &Path, args: &[OsString]) -> i32 {
    pass_through(program, args)
}

fn pass_through(program: &Path, args: &[OsString]) -> i32 {
    exec::pass_through(program, args).unwrap_or_else(|error| {
        eprintln!("tinymold: {error}");
        1
    })
}

fn utf8(args: &[OsString]) -> Option<Vec<String>> {
    args.iter()
        .map(|arg| arg.to_str().map(str::to_owned))
        .collect()
}

fn log(config: &Config, started: Instant, outcome: &str, crate_name: &str, detail: &str) {
    let Some(path) = &config.log else {
        return;
    };
    let line = format!(
        "{outcome} {crate_name} {:.1}ms {detail}\n",
        started.elapsed().as_secs_f64() * 1000.0
    );
    // One write per line keeps concurrent wrappers' lines whole.
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod test;
