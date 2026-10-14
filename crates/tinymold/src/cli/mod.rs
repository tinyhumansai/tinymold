//! Argument definitions and command dispatch.
//!
//! [`Cli`] is the parsed command line and [`run`] executes it against any
//! writer, so every command is tested by passing a `Vec<u8>` instead of
//! spawning the binary.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use tinymold_cas::{Store, Strategy};
use tinymold_hash::Digest;

use crate::doctor::Report;
use crate::{Error, Result, default_store};

/// The `tinymold` command line.
#[derive(Debug, Parser)]
#[command(name = "tinymold", version, about, long_about = None)]
pub struct Cli {
    /// The store directory. Defaults to `TINYMOLD_STORE`, then
    /// `XDG_CACHE_HOME/tinymold`, then `HOME/.cache/tinymold`.
    #[arg(long, global = true, value_name = "DIR")]
    pub store: Option<PathBuf>,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum Command {
    /// Print the BLAKE3 digest of each file, in `b3sum` format.
    Hash {
        /// Files to hash.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Work with the local content-addressed store.
    #[command(subcommand)]
    Cas(CasCommand),
    /// Report the host, toolchain, store, and installed fast linkers.
    Doctor,
}

/// Content-addressed store commands.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum CasCommand {
    /// Store files and print each one's digest.
    Put {
        /// Files to store.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Place a stored object at a path and print how it was placed.
    Get {
        /// How the object may be placed.
        #[arg(long, value_enum, default_value_t = StrategyArg::Auto)]
        strategy: StrategyArg,
        /// The object's digest.
        digest: Digest,
        /// Where to place it; must not exist.
        destination: PathBuf,
    },
    /// Exit 0 if the object is stored, 1 if it is not.
    Has {
        /// The object's digest.
        digest: Digest,
    },
    /// Re-hash a stored object and fail if it is corrupt.
    Verify {
        /// The object's digest.
        digest: Digest,
    },
}

/// The `--strategy` values accepted by `tinymold cas get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StrategyArg {
    /// Reflink, then hardlink, then copy.
    Auto,
    /// Copy-on-write clone only.
    Reflink,
    /// Hardlink only; the result is read-only.
    Hardlink,
    /// Byte copy only.
    Copy,
}

impl From<StrategyArg> for Strategy {
    fn from(value: StrategyArg) -> Self {
        match value {
            StrategyArg::Auto => Self::Auto,
            StrategyArg::Reflink => Self::Reflink,
            StrategyArg::Hardlink => Self::Hardlink,
            StrategyArg::Copy => Self::Copy,
        }
    }
}

/// How a successful command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The command did what was asked.
    Success,
    /// The command ran, but the answer is "no" (for example `cas has` on an
    /// absent object). Exits with status 1, like `grep` without a match.
    Absent,
}

impl From<Outcome> for ExitCode {
    fn from(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Success => Self::SUCCESS,
            Outcome::Absent => Self::from(1),
        }
    }
}

/// Runs `cli`, writing its normal output to `out`.
///
/// # Errors
///
/// Returns [`Error::NoStoreLocation`] when a store command has no store,
/// [`Error::Output`] when `out` cannot be written, and the hashing or store
/// error of whatever operation failed.
pub fn run(cli: &Cli, out: &mut impl Write) -> Result<Outcome> {
    match &cli.command {
        Command::Hash { files } => {
            for file in files {
                let digest = Digest::of_file(file)?;
                line(out, format_args!("{digest}  {}", file.display()))?;
            }
        }
        Command::Cas(command) => return run_cas(command, &open_store(cli)?, out),
        Command::Doctor => {
            let path = std::env::var_os("PATH");
            let report = Report::gather(path.as_deref(), Path::new("rustc"), store_dir(cli));
            write!(out, "{report}").map_err(Error::Output)?;
        }
    }
    Ok(Outcome::Success)
}

fn run_cas(command: &CasCommand, store: &Store, out: &mut impl Write) -> Result<Outcome> {
    match command {
        CasCommand::Put { files } => {
            for file in files {
                let digest = store.insert_file(file)?;
                line(out, format_args!("{digest}  {}", file.display()))?;
            }
        }
        CasCommand::Get {
            strategy,
            digest,
            destination,
        } => {
            let method = store.materialize(digest, destination, (*strategy).into())?;
            line(out, format_args!("{method}  {}", destination.display()))?;
        }
        CasCommand::Has { digest } => {
            if !store.contains(digest) {
                return Ok(Outcome::Absent);
            }
        }
        CasCommand::Verify { digest } => {
            store.verify(digest)?;
            line(out, format_args!("ok  {digest}"))?;
        }
    }
    Ok(Outcome::Success)
}

fn store_dir(cli: &Cli) -> Option<PathBuf> {
    cli.store
        .clone()
        .or_else(|| default_store(|name| std::env::var_os(name).map(OsString::from)))
}

fn open_store(cli: &Cli) -> Result<Store> {
    let dir = store_dir(cli).ok_or(Error::NoStoreLocation)?;
    Ok(Store::open(dir)?)
}

fn line(out: &mut impl Write, text: std::fmt::Arguments<'_>) -> Result<()> {
    writeln!(out, "{text}").map_err(Error::Output)
}

#[cfg(test)]
mod test;
