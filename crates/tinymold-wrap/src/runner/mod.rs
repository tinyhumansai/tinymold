//! Serving one test run: replay a recorded pass, or run and record.
//!
//! Cargo's `runner` setting makes cargo run `tinymold run-test <binary>
//! <args…>` for every test binary, from the package root. The steps follow
//! `docs/specs/test-cache.md`:
//!
//! 1. decide whether the run may be cached: not a bench or coverage run, not
//!    opted out, and a binary the wrapper built from a `--test` compile;
//! 2. compute the test key from the binary's digest, the arguments, the
//!    host, the package tree, the declared inputs and the environment;
//! 3. on a hit, replay the recorded stdout and stderr and exit 0;
//! 4. on a miss, run the binary with its output streamed through, and
//!    record the run if it passed and no file under the package changed
//!    while it ran.
//!
//! Whatever goes wrong inside the cache, the test runs as cargo asked.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use tinymold_cas::Store;
use tinymold_hash::Digest;
use tinymold_key::{Roots, TestKeyInputs, test_key, tree_digest};

use crate::clock::filesystem_now;
use crate::declared::{self, Declared};
use crate::exec;
use crate::package_tree::{declared_files, package_files};
use crate::record::{self, write_atomically};
use crate::stat;
use crate::test_record::{TestBinary, TestRun};
use crate::{Config, Result};

/// One test binary run, as cargo asked for it.
#[derive(Debug, Clone, Copy)]
pub struct TestCommand<'a> {
    /// A program to run the binary through, such as an emulator, from
    /// `tinymold run-test --via <program>`; `None` to run it directly.
    pub via: Option<&'a Path>,
    /// The test binary.
    pub binary: &'a Path,
    /// The arguments after the binary.
    pub args: &'a [OsString],
    /// The working directory: the package root, where cargo runs tests.
    pub cwd: &'a Path,
    /// The whole environment the binary runs with.
    pub env: &'a [(OsString, OsString)],
}

/// Runs one test binary, replaying a recorded pass when nothing the run
/// can observe has changed, and returns the exit code cargo should see.
///
/// Cache failures never change a test's outcome: the binary then runs as
/// cargo asked. Only a binary that cannot be started at all is reported,
/// with exit code 1.
#[must_use]
pub fn run_test(config: &Config, command: &TestCommand<'_>) -> i32 {
    let started = Instant::now();
    match prepare(&config.store, command) {
        Ok(Ok(prepared)) => serve(config, command, &prepared, started),
        Ok(Err(reason)) => {
            crate::log(config, started, "test-pass", "-", reason);
            run_plain(command)
        }
        Err(error) => {
            crate::log(config, started, "test-error", "-", &error.to_string());
            run_plain(command)
        }
    }
}

/// What serving a cacheable run needs.
struct Prepared {
    store: Store,
    key: Digest,
    crate_name: String,
    binary_record: PathBuf,
    /// The package tree and listing the key was computed from, to check
    /// after the run.
    observed: (Digest, Digest),
    declared: Vec<String>,
    dylibs: Vec<String>,
    now_ns: u64,
}

fn serve(config: &Config, command: &TestCommand<'_>, prepared: &Prepared, started: Instant) -> i32 {
    let record_path = TestRun::path(&config.store, &prepared.key);
    if let Ok(Some(recorded)) = replay(&prepared.store, &record_path) {
        record::touch(&record_path);
        record::touch(&prepared.binary_record);
        crate::log(config, started, "test-hit", &prepared.crate_name, "");
        return recorded;
    }
    let (program, args) = program_and_args(command);
    let captured = match exec::capture(build(command, program, &args), program) {
        Ok(captured) => captured,
        Err(error) => {
            eprintln!("tinymold: {error}");
            return 1;
        }
    };
    let detail = if captured.code == 0 {
        let recording = Instant::now();
        match record_run(config, command, prepared, &record_path, &captured) {
            Ok(Ok(())) => format!(
                "recorded in {:.1}ms",
                recording.elapsed().as_secs_f64() * 1000.0
            ),
            Ok(Err(reason)) => format!("not recorded: {reason}"),
            Err(error) => format!("not recorded: {error}"),
        }
    } else {
        "not recorded: the run failed".to_owned()
    };
    crate::log(config, started, "test-miss", &prepared.crate_name, &detail);
    captured.code
}

/// Replays the run recorded at `path`, returning its exit code, or `None`
/// when there is no record. Nothing is written until both streams are read,
/// so a missing object falls back to running the test.
fn replay(store: &Store, path: &Path) -> Result<Option<i32>> {
    let Some(text) = record::read(path)? else {
        return Ok(None);
    };
    let run = TestRun::decode(&text, path)?;
    let stdout = store.read(&run.stdout)?;
    let stderr = store.read(&run.stderr)?;
    let _ = io::stdout().lock().write_all(&stdout);
    let _ = io::stderr().lock().write_all(&stderr);
    Ok(Some(0))
}

/// Stores a passing run, unless a file it may have read changed while it
/// ran: then the key no longer says what the test saw.
fn record_run(
    config: &Config,
    command: &TestCommand<'_>,
    prepared: &Prepared,
    path: &Path,
    captured: &exec::Captured,
) -> Result<std::result::Result<(), &'static str>> {
    let after = observed(
        &config.store,
        command,
        &prepared.declared,
        &prepared.dylibs,
        prepared.now_ns,
    )?;
    if after != prepared.observed {
        return Ok(Err("a file under the package changed during the run"));
    }
    let run = TestRun {
        stdout: prepared.store.insert_bytes(&captured.stdout)?,
        stderr: prepared.store.insert_bytes(&captured.stderr)?,
    };
    write_atomically(path, &run.encode())?;
    Ok(Ok(()))
}

/// The test key of a cacheable run, or why the run is not cacheable.
fn prepare(
    store_dir: &Path,
    command: &TestCommand<'_>,
) -> Result<std::result::Result<Prepared, &'static str>> {
    let Some(args) = utf8(command.args.iter()) else {
        return Ok(Err("an argument is not UTF-8"));
    };
    let mut env = Vec::with_capacity(command.env.len());
    for (name, value) in command.env {
        let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
            return Ok(Err("an environment variable is not UTF-8"));
        };
        env.push((name.to_owned(), value.to_owned()));
    }
    let var = |wanted: &str| {
        env.iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.as_str())
    };
    if var("TINYMOLD_TEST_CACHE") == Some("0") {
        return Ok(Err("TINYMOLD_TEST_CACHE=0"));
    }
    if args.iter().any(|arg| arg == "--bench") {
        return Ok(Err("a bench run"));
    }
    if var("LLVM_PROFILE_FILE").is_some() {
        return Ok(Err("a coverage run"));
    }

    let store = Store::open(store_dir)?;
    let now_ns = filesystem_now(&store_dir.join("stat"))?;
    let binary = command.cwd.join(command.binary);
    let digest = stat::digest(store_dir, &binary, now_ns)?;
    let binary_record = TestBinary::path(store_dir, &digest);
    let Some(text) = record::read(&binary_record)? else {
        return Ok(Err("not a test binary tinymold built"));
    };
    let built = TestBinary::decode(&text, &binary_record)?;
    let Some(manifest) = record::read(&command.cwd.join("Cargo.toml"))? else {
        return Ok(Err("no Cargo.toml in the working directory"));
    };
    let declared = match declared::parse(&manifest) {
        Declared::Inputs(paths) => paths,
        Declared::Uncached(reason) => return Ok(Err(reason)),
    };

    let (package_tree, files) = observed(store_dir, command, &declared, &built.dylibs, now_ns)?;
    let binary_dir = binary.parent().unwrap_or(command.cwd);
    let cargo_home = var("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|home| Path::new(home).join(".cargo")));
    let roots = Roots::new(command.cwd, binary_dir, cargo_home.as_deref());
    let via = command
        .via
        .map(|via| stat::linker_identity(via, command.cwd));
    let key = test_key(&TestKeyInputs {
        binary: &digest,
        via: via.as_deref(),
        args: &args,
        roots: &roots,
        host: &host(),
        package_tree: &package_tree,
        declared_inputs: &files,
        env: &env,
    });
    Ok(Ok(Prepared {
        store,
        key,
        crate_name: built.crate_name,
        binary_record,
        observed: (package_tree, files),
        declared,
        dylibs: built.dylibs,
        now_ns,
    }))
}

/// What a run may read besides its binary: the digest of the package tree,
/// and that of the declared inputs together with the dynamic libraries the
/// binary loads, which its own bytes do not cover.
fn observed(
    store_dir: &Path,
    command: &TestCommand<'_>,
    declared: &[String],
    dylibs: &[String],
    now_ns: u64,
) -> Result<(Digest, Digest)> {
    let binary = command.cwd.join(command.binary);
    let package_tree = tree_digest(&package_files(store_dir, command.cwd, &binary, now_ns)?);
    let mut files = declared_files(store_dir, command.cwd, declared, now_ns)?;
    let dir = binary.parent().unwrap_or(command.cwd);
    for name in dylibs {
        files.push((
            format!("dylib/{name}"),
            stat::digest(store_dir, &dir.join(name), now_ns)?,
        ));
    }
    Ok((package_tree, tree_digest(&files)))
}

/// The host a test runs on: OS, architecture, and the OS release, since
/// tests call system libraries the binary's digest does not cover.
fn host() -> String {
    let release = if cfg!(target_os = "linux") {
        fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        fs::read("/System/Library/CoreServices/SystemVersion.plist")
            .map(|plist| Digest::of_bytes(&plist).to_hex())
            .unwrap_or_default()
    } else {
        String::new()
    };
    format!(
        "{} {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        release.trim()
    )
}

/// The program to start and its arguments: the binary itself, or the
/// `--via` program with the binary as its first argument.
fn program_and_args<'a>(command: &TestCommand<'a>) -> (&'a Path, Vec<OsString>) {
    match command.via {
        Some(via) => {
            let mut args = vec![command.binary.as_os_str().to_owned()];
            args.extend(command.args.iter().cloned());
            (via, args)
        }
        None => (command.binary, command.args.to_vec()),
    }
}

/// The process cargo asked for: the same program, arguments, working
/// directory and environment.
fn build(command: &TestCommand<'_>, program: &Path, args: &[OsString]) -> Command {
    let mut process = Command::new(program);
    process
        .args(args)
        .current_dir(command.cwd)
        .env_clear()
        .envs(command.env.iter().map(|(name, value)| (name, value)));
    process
}

/// Runs one test binary unchanged, as cargo asked, and returns its exit
/// code, or 1 when it cannot be started. Used when no store is available.
#[must_use]
pub fn run_test_uncached(command: &TestCommand<'_>) -> i32 {
    run_plain(command)
}

/// Runs the test unchanged and returns its exit code, or 1 when it cannot
/// be started.
fn run_plain(command: &TestCommand<'_>) -> i32 {
    let (program, args) = program_and_args(command);
    exec::pass_through_command(build(command, program, &args), program).unwrap_or_else(|error| {
        eprintln!("tinymold: {error}");
        1
    })
}

fn utf8<'a>(args: impl Iterator<Item = &'a OsString>) -> Option<Vec<String>> {
    args.map(|arg| arg.to_str().map(str::to_owned)).collect()
}

#[cfg(test)]
mod test;
