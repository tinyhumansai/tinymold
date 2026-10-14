//! Argument definitions and command dispatch.
//!
//! [`Cli`] is the parsed command line and [`run`] executes it against any
//! writer, so every command is tested by passing a `Vec<u8>` instead of
//! spawning the binary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use tinymold_cas::{Store, Strategy};
use tinymold_hash::Digest;

use crate::doctor::Report;
use crate::fresh::{self, Restore};
use crate::{Error, Result, default_store, snapshot};

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
    /// Undo mtime changes that did not change file content, so cargo does
    /// not rebuild after a checkout, switch, or touch.
    #[command(subcommand)]
    Fresh(FreshCommand),
    /// Save and restore whole `target/` snapshots, so a new worktree or a
    /// branch switch starts warm.
    #[command(subcommand)]
    Snapshot(SnapshotCommand),
}

/// Freshness commands.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum FreshCommand {
    /// Record every workspace file's mtime and digest; run after a build.
    Record(Location),
    /// Put back recorded mtimes on files whose content is unchanged; run
    /// before a build. Exits 1 when there is no usable manifest.
    Restore(Location),
}

/// Snapshot commands.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum SnapshotCommand {
    /// Record freshness and save a clone of the target directory.
    Save {
        /// Snapshot name; defaults to the workspace's git HEAD commit.
        name: Option<String>,
        /// The workspace and target directory.
        #[command(flatten)]
        location: Location,
    },
    /// Clone a snapshot into the target directory and make unchanged files
    /// fresh against it.
    Restore {
        /// Snapshot name; defaults to the one matching the most files.
        name: Option<String>,
        /// Replace an existing target directory instead of failing.
        #[arg(long)]
        replace: bool,
        /// The workspace and target directory.
        #[command(flatten)]
        location: Location,
    },
    /// List saved snapshots.
    List,
}

/// Which workspace and target directory a command works on.
#[derive(Debug, Clone, Args)]
pub struct Location {
    /// The workspace root. Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
    /// The cargo target directory. Defaults to `CARGO_TARGET_DIR`, then
    /// `<workspace>/target`.
    #[arg(long, value_name = "DIR")]
    pub target_dir: Option<PathBuf>,
}

impl Location {
    /// The workspace and target directories this location names, reading
    /// `CARGO_TARGET_DIR` through `lookup`. A relative target directory is
    /// taken relative to the workspace, as cargo does.
    fn resolve(&self, lookup: impl Fn(&str) -> Option<std::ffi::OsString>) -> (PathBuf, PathBuf) {
        let workspace = self.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
        let target = self
            .target_dir
            .clone()
            .or_else(|| {
                lookup("CARGO_TARGET_DIR")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            })
            .map_or_else(|| workspace.join("target"), |dir| workspace.join(dir));
        (workspace, target)
    }
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
        Command::Fresh(command) => return run_fresh(command, out),
        Command::Snapshot(command) => return run_snapshot(cli, command, out),
    }
    Ok(Outcome::Success)
}

fn env(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name)
}

fn run_fresh(command: &FreshCommand, out: &mut impl Write) -> Result<Outcome> {
    match command {
        FreshCommand::Record(location) => {
            let (workspace, target) = location.resolve(env);
            let report = fresh::record(&workspace, &target)?;
            line(
                out,
                format_args!(
                    "recorded {} files ({} hashed, {} skipped) in {}",
                    report.files,
                    report.hashed,
                    report.skipped,
                    fresh::manifest_path(&target).display()
                ),
            )?;
            Ok(Outcome::Success)
        }
        FreshCommand::Restore(location) => {
            let (workspace, target) = location.resolve(env);
            report_restore(fresh::restore(&workspace, &target)?, &target, out)
        }
    }
}

/// Prints a freshness restore; a restore that could not run is
/// [`Outcome::Absent`] so scripts can tell.
fn report_restore(restore: Restore, target: &Path, out: &mut impl Write) -> Result<Outcome> {
    match restore {
        Restore::Applied(report) => {
            line(
                out,
                format_args!(
                    "fresh: {} restored, {} unchanged, {} changed, {} missing",
                    report.restored, report.unchanged, report.changed, report.missing
                ),
            )?;
            Ok(Outcome::Success)
        }
        Restore::NoManifest => {
            line(
                out,
                format_args!(
                    "fresh: no manifest in {}; run tinymold fresh record after a build",
                    target.display()
                ),
            )?;
            Ok(Outcome::Absent)
        }
        Restore::TargetChanged => {
            line(
                out,
                format_args!("fresh: the target changed since the last record; nothing restored"),
            )?;
            Ok(Outcome::Absent)
        }
    }
}

fn run_snapshot(cli: &Cli, command: &SnapshotCommand, out: &mut impl Write) -> Result<Outcome> {
    let store = store_dir(cli).ok_or(Error::NoStoreLocation)?;
    match command {
        SnapshotCommand::Save { name, location } => {
            let (workspace, target) = location.resolve(env);
            let name = match name {
                Some(name) => name.clone(),
                None => git_head(&workspace).ok_or(Error::NoSnapshotName)?,
            };
            let clone = snapshot::save(&store, &name, &workspace, &target)?;
            line(out, format_args!("saved snapshot {name} ({clone})"))?;
            Ok(Outcome::Success)
        }
        SnapshotCommand::Restore {
            name,
            replace,
            location,
        } => {
            let (workspace, target) = location.resolve(env);
            let restored =
                snapshot::restore(&store, name.as_deref(), &workspace, &target, *replace)?;
            line(
                out,
                format_args!(
                    "restored snapshot {} ({}; {} of {} files match)",
                    restored.name, restored.clone, restored.matched, restored.files
                ),
            )?;
            if let Some(kept) = &restored.kept {
                line(
                    out,
                    format_args!("kept the previous target as snapshot {kept}"),
                )?;
            }
            report_restore(restored.fresh, &target, out)
        }
        SnapshotCommand::List => {
            for snapshot in snapshot::list(&store)? {
                line(
                    out,
                    format_args!("{}  {} files", snapshot.name, snapshot.files),
                )?;
            }
            Ok(Outcome::Success)
        }
    }
}

/// The commit checked out in `workspace`, or `None` outside a git checkout.
fn git_head(workspace: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    let head = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !head.is_empty()).then_some(head)
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
        .or_else(|| default_store(|name| std::env::var_os(name)))
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
