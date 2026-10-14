//! `tinymold doctor`: what this machine has for fast Rust builds.
//!
//! The doctor reports facts, not advice: the host, the Rust toolchain, the
//! store location and size, and which fast linkers and caches are installed. Linking
//! is roughly half of an incremental debug rebuild (see
//! `docs/research/linkers.md`), so knowing whether mold, wild, or lld is
//! available is the first thing any speed-up depends on. Diagnosing a
//! specific build comes in a later phase of the roadmap.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::gc::{self, StoreUsage};

/// One external tool the doctor looks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// The executable name searched for.
    pub name: &'static str,
    /// What tinymold would use it for.
    pub purpose: &'static str,
    /// Where it was found, if anywhere.
    pub path: Option<PathBuf>,
}

/// Everything `tinymold doctor` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// This tinymold's version.
    pub version: &'static str,
    /// The host operating system, as `std::env::consts::OS` names it.
    pub os: &'static str,
    /// The host architecture, as `std::env::consts::ARCH` names it.
    pub arch: &'static str,
    /// The first line of `rustc -vV`, or `None` when rustc did not run.
    pub rustc: Option<String>,
    /// The `rust-lld` shipped in the toolchain's sysroot, if present.
    pub rust_lld: Option<PathBuf>,
    /// The store directory tinymold would use.
    pub store: Option<PathBuf>,
    /// How much the store holds, when it could be read.
    pub store_usage: Option<StoreUsage>,
    /// The fast linkers and caches searched for on `PATH`.
    pub tools: Vec<Tool>,
}

/// The tools searched for, with the reason each matters.
const TOOLS: [(&str, &str); 6] = [
    ("mold", "parallel ELF linker"),
    ("wild", "parallel ELF linker aiming at incremental links"),
    ("ld.lld", "LLVM linker for ELF"),
    ("ld64.lld", "LLVM linker for Mach-O"),
    ("clang", "linker driver that accepts -fuse-ld"),
    ("sccache", "compiler cache tinymold compares against"),
];

impl Report {
    /// Gathers a report by searching `path` (a `PATH`-style list) and asking
    /// the `rustc` executable for its version and sysroot.
    #[must_use]
    pub fn gather(path: Option<&OsStr>, rustc: &Path, store: Option<PathBuf>) -> Self {
        let tools = TOOLS
            .iter()
            .map(|&(name, purpose)| Tool {
                name,
                purpose,
                path: path.and_then(|path| find_on_path(name, path)),
            })
            .collect();
        let version = rustc_output(rustc, &["-vV"]);
        let host = version.as_deref().and_then(|text| field(text, "host: "));
        let rust_lld = host.and_then(|host| {
            let sysroot = rustc_output(rustc, &["--print", "sysroot"])?;
            let lld = Path::new(sysroot.trim())
                .join("lib/rustlib")
                .join(host)
                .join("bin")
                .join(executable("rust-lld"));
            lld.is_file().then_some(lld)
        });

        Self {
            version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            rustc: version.and_then(|text| text.lines().next().map(str::to_owned)),
            rust_lld,
            store_usage: store.as_deref().and_then(|store| gc::usage(store).ok()),
            store,
            tools,
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let found = |path: Option<&PathBuf>| {
            path.map_or_else(|| "not found".to_owned(), |path| path.display().to_string())
        };

        writeln!(f, "tinymold {}", self.version)?;
        writeln!(f, "host      {} {}", self.os, self.arch)?;
        writeln!(
            f,
            "rustc     {}",
            self.rustc.as_deref().unwrap_or("not found")
        )?;
        writeln!(f, "rust-lld  {}", found(self.rust_lld.as_ref()))?;
        match &self.store_usage {
            Some(usage) => writeln!(f, "store     {} ({usage})", found(self.store.as_ref()))?,
            None => writeln!(f, "store     {}", found(self.store.as_ref()))?,
        }
        writeln!(f)?;
        writeln!(f, "linkers and caches on PATH")?;
        for tool in &self.tools {
            writeln!(
                f,
                "  {:<9} {:<50} {}",
                tool.name,
                found(tool.path.as_ref()),
                tool.purpose
            )?;
        }
        Ok(())
    }
}

/// Finds the executable `name` in a `PATH`-style list of directories.
///
/// ```
/// let dir = tempfile::tempdir()?;
/// std::fs::write(dir.path().join("mold"), b"")?;
/// let path = std::env::join_paths([dir.path()])?;
/// assert!(tinymold::find_on_path("mold", &path).is_some() || cfg!(windows));
/// assert_eq!(tinymold::find_on_path("wild", &path), None);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn find_on_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(executable(name)))
        .find(|candidate| candidate.is_file())
}

fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Runs `rustc` with `args` and returns its stdout when it succeeds.
fn rustc_output(rustc: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(rustc).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Returns the value of the `prefix` line in `rustc -vV` output.
fn field<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines().find_map(|line| line.strip_prefix(prefix))
}

#[cfg(test)]
mod test;
