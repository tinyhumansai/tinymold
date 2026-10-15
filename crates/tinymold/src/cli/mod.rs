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
use crate::gc::{self, Age, ByteSize};
use crate::stats::Stats;
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
    /// Remove cache entries unused for a while or beyond a size budget, and
    /// objects nothing references any more.
    Gc(GcArgs),
    /// Summarize the wrapper log: hits, misses, and time spent.
    Stats(StatsArgs),
    /// Share the cache through a remote REAPI server (`TINYMOLD_REMOTE_URL`).
    #[command(subcommand)]
    Remote(RemoteCommand),
    /// Run a test binary, replaying a recorded pass when nothing the run can
    /// observe has changed. Set it as cargo's runner:
    /// `[target.'cfg(all())'] runner = ["tinymold", "run-test"]`.
    RunTest(RunTestArgs),
}

/// What `tinymold run-test` runs.
#[derive(Debug, Clone, Args)]
pub struct RunTestArgs {
    /// Run the binary through this program, such as an emulator; its
    /// identity becomes part of the cache key.
    #[arg(long, value_name = "PROGRAM")]
    pub via: Option<PathBuf>,
    /// The test binary, as cargo passes it.
    pub binary: PathBuf,
    /// The binary's arguments, passed on unchanged.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<std::ffi::OsString>,
}

/// Remote cache commands.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum RemoteCommand {
    /// Upload the actions and input sets used since `--since` (all without
    /// it), with every object the server lacks.
    Push {
        /// Only records used at or after this time, in Unix seconds, such as
        /// the start of the CI job.
        #[arg(long, value_name = "UNIX_SECONDS")]
        since: Option<u64>,
    },
    /// Check that the server is reachable and accepts the credential, and
    /// print what it supports.
    Check,
}

/// What `tinymold stats` reads and how it reports.
#[derive(Debug, Clone, Args)]
pub struct StatsArgs {
    /// The wrapper log. Defaults to `TINYMOLD_LOG`; a log that does not
    /// exist yet counts as empty.
    #[arg(long, value_name = "FILE")]
    pub log: Option<PathBuf>,
    /// Print a Markdown table, for a CI job summary.
    #[arg(long)]
    pub markdown: bool,
    /// Exit 1 when fewer than this percentage of cacheable compiles hit.
    #[arg(long, value_name = "PERCENT")]
    pub min_hit_rate: Option<f64>,
}

/// The limits of `tinymold gc`.
#[derive(Debug, Clone, Args)]
pub struct GcArgs {
    /// Remove entries not used for this long, such as `14d` or `36h`.
    #[arg(long, value_name = "AGE")]
    pub max_age: Option<Age>,
    /// Keep at most this many entries per compiled unit, the most recently
    /// used; older edits of the same crate go.
    #[arg(long, value_name = "N")]
    pub keep_generations: Option<usize>,
    /// Evict the least recently used entries while the store is larger than
    /// this, such as `10G`.
    #[arg(long, value_name = "SIZE")]
    pub max_size: Option<ByteSize>,
    /// Remove `target/` snapshots saved longer ago than this.
    #[arg(long, value_name = "AGE")]
    pub snapshots_max_age: Option<Age>,
    /// Leave unreferenced objects and temporaries younger than this, in case
    /// a build is storing them right now (default `1h`). Lower it only when
    /// no build is running, as at the end of a CI job.
    #[arg(long, value_name = "AGE")]
    pub grace: Option<Age>,
    /// Report what would be removed without removing it.
    #[arg(long)]
    pub dry_run: bool,
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
    /// The command ran another program, whose exit status this is
    /// (`run-test`).
    Exit(u8),
}

impl From<Outcome> for ExitCode {
    fn from(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Success => Self::SUCCESS,
            Outcome::Absent => Self::from(1),
            Outcome::Exit(code) => Self::from(code),
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
            let mut report = Report::gather(path.as_deref(), Path::new("rustc"), store_dir(cli));
            report.remote = remote_summary(env);
            write!(out, "{report}").map_err(Error::Output)?;
        }
        Command::Fresh(command) => return run_fresh(command, out),
        Command::Snapshot(command) => return run_snapshot(cli, command, out),
        Command::Gc(args) => {
            let store = store_dir(cli).ok_or(Error::NoStoreLocation)?;
            let policy = gc::Policy {
                max_age: args.max_age.map(|age| age.0),
                keep_generations: args.keep_generations,
                max_size: args.max_size.map(|size| size.0),
                snapshots_max_age: args.snapshots_max_age.map(|age| age.0),
                dry_run: args.dry_run,
                grace: args.grace.map_or(gc::GRACE, |age| age.0),
                ..gc::Policy::garbage_only()
            };
            let report = gc::collect(&store, &policy)?;
            line(out, format_args!("{report}"))?;
        }
        Command::Stats(args) => return run_stats(args, out),
        Command::Remote(command) => return run_remote(cli, command, env, out),
        Command::RunTest(args) => return Ok(run_test(cli, args)),
    }
    Ok(Outcome::Success)
}

/// Runs a test binary through the test result cache, or unchanged when no
/// store can be found. The binary's output goes straight to this process's
/// stdout and stderr, as cargo expects of a runner.
fn run_test(cli: &Cli, args: &RunTestArgs) -> Outcome {
    // Cargo runs tests in the package root; should the absolute path be
    // unreadable, `.` still names it, only without normalizing paths.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let vars: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os().collect();
    let command = tinymold_wrap::TestCommand {
        via: args.via.as_deref(),
        binary: &args.binary,
        args: &args.args,
        cwd: &cwd,
        env: &vars,
        stdin_has_input: tinymold_wrap::stdin_has_input(),
    };
    let code = match crate::wrapper_config(env) {
        Some(mut config) => {
            if let Some(store) = &cli.store {
                config.store.clone_from(store);
            }
            tinymold_wrap::run_test(&config, &command)
        }
        None => match &cli.store {
            Some(store) => tinymold_wrap::run_test(&tinymold_wrap::Config::new(store), &command),
            None => tinymold_wrap::run_test_uncached(&command),
        },
    };
    Outcome::Exit(u8::try_from(code).unwrap_or(1))
}

fn run_stats(args: &StatsArgs, out: &mut impl Write) -> Result<Outcome> {
    let log = args
        .log
        .clone()
        .or_else(|| {
            env("TINYMOLD_LOG")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
        .ok_or(Error::NoLogLocation)?;
    let text = match std::fs::read_to_string(&log) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(Error::Io {
                action: "read",
                path: log,
                source,
            });
        }
    };
    let stats = Stats::parse(&text);
    if args.markdown {
        write!(out, "{}", stats.markdown()).map_err(Error::Output)?;
    } else {
        line(out, format_args!("{stats}"))?;
    }
    let enough = args
        .min_hit_rate
        .is_none_or(|min| stats.hit_rate().is_some_and(|rate| rate >= min));
    Ok(if enough {
        Outcome::Success
    } else {
        Outcome::Absent
    })
}

/// What `doctor` says about the remote tier: its URL, or why it is unusable.
fn remote_summary(lookup: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<String> {
    let url = lookup("TINYMOLD_REMOTE_URL").filter(|v| !v.is_empty())?;
    let url = url.to_string_lossy().into_owned();
    #[cfg(feature = "remote")]
    let summary = match tinymold_remote::RemoteConfig::from_env(lookup) {
        Ok(Some(config)) if config.authorization.is_some() => format!("{url} (with credential)"),
        Ok(_) => format!("{url} (anonymous)"),
        Err(error) => format!("{url} ({error})"),
    };
    #[cfg(not(feature = "remote"))]
    let summary = format!("{url} (ignored: built without the remote feature)");
    Some(summary)
}

/// Runs a remote command against the server the `TINYMOLD_REMOTE_*`
/// variables, read through `lookup`, name.
#[cfg(feature = "remote")]
fn run_remote(
    cli: &Cli,
    command: &RemoteCommand,
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
    out: &mut impl Write,
) -> Result<Outcome> {
    let config = tinymold_remote::RemoteConfig::from_env(lookup)?.ok_or(Error::NoRemote)?;
    let remote = tinymold_remote::RemoteCache::new(&config)?;
    match command {
        RemoteCommand::Push { since } => {
            let store = store_dir(cli).ok_or(Error::NoStoreLocation)?;
            let since = since
                .map(|seconds| std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds));
            let report = remote.push(&store, since)?;
            line(out, format_args!("remote: {report}"))?;
        }
        RemoteCommand::Check => {
            let capabilities = remote.check()?;
            line(
                out,
                format_args!("remote: {} reachable; {capabilities}", config.endpoint),
            )?;
        }
    }
    Ok(Outcome::Success)
}

#[cfg(not(feature = "remote"))]
fn run_remote(
    _cli: &Cli,
    _command: &RemoteCommand,
    _lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
    _out: &mut impl Write,
) -> Result<Outcome> {
    Err(Error::NoRemote)
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
