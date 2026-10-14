//! Compiler drivers that wrap rustc, and what they add to a unit's key.
//!
//! `cargo clippy` sets `RUSTC_WORKSPACE_WRAPPER=clippy-driver`, so for
//! workspace crates cargo runs `tinymold clippy-driver <rustc> <args…>`.
//! clippy-driver is rustc with lints registered on top: it takes the same
//! arguments, writes the same outputs (an `.rmeta` and dep-info for a check)
//! and reports its lints as diagnostics, which a hit replays. So it is
//! cached like rustc, with its own identity in the pre-key, which keeps its
//! entries apart from plain rustc's.
//!
//! clippy-driver's dep-info already records what its lints read: the
//! configuration file it found, `Cargo.toml` (for the `[lints]` table) and
//! the `CLIPPY_ARGS` and `CLIPPY_CONF_DIR` variables, so the full key covers
//! them. A configuration file that does not exist yet is not in any
//! dep-info, though, and a worktree that has one must not hit an entry made
//! without it. So the pre-key also names the configuration file clippy
//! would find, and its digest, or that there is none.

use std::path::{Path, PathBuf};

use tinymold_key::Roots;

use crate::Result;
use crate::stat;

/// The file names clippy looks for in each directory, in its order.
const CONFIG_FILE_NAMES: [&str; 2] = [".clippy.toml", "clippy.toml"];

/// Whether `program` with `args` is clippy-driver wrapping rustc: a file
/// named `clippy-driver` whose first argument is a file named `rustc`.
pub(crate) fn is_clippy<S: AsRef<str>>(program: &Path, args: &[S]) -> bool {
    program
        .file_stem()
        .is_some_and(|stem| stem == "clippy-driver")
        && args
            .first()
            .is_some_and(|first| Path::new(first.as_ref()).file_stem().is_some_and(|s| s == "rustc"))
}

/// Where clippy starts looking for its configuration: `CLIPPY_CONF_DIR`,
/// else the crate's `CARGO_MANIFEST_DIR`, else the current directory `cwd`.
pub(crate) fn config_start(cwd: &Path) -> PathBuf {
    std::env::var_os("CLIPPY_CONF_DIR")
        .or_else(|| std::env::var_os("CARGO_MANIFEST_DIR"))
        .map_or_else(|| cwd.to_path_buf(), |dir| cwd.join(dir))
}

/// What clippy-driver adds to the pre-key: its version and the
/// configuration file it would find looking from `start`.
///
/// # Errors
///
/// Returns an error when clippy-driver cannot be run or a configuration file
/// cannot be hashed.
pub(crate) fn clippy_identity(
    store: &Path,
    program: &Path,
    start: &Path,
    roots: &Roots,
    now_ns: u64,
) -> Result<String> {
    let version = stat::version(store, program, "--version")?;
    let config = match find_config(start) {
        Some(path) => {
            let digest = stat::digest(store, &path, now_ns)?;
            format!("{} {digest}", roots.normalize(&path.to_string_lossy()))
        }
        None => "none".to_owned(),
    };
    Ok(format!("clippy-driver\n{}\nconfig {config}", version.trim_end()))
}

/// The configuration file clippy reads when it starts looking in `start`:
/// the first of [`CONFIG_FILE_NAMES`] in `start` or its nearest ancestor
/// holding one.
fn find_config(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|dir| {
        CONFIG_FILE_NAMES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.is_file())
    })
}

#[cfg(test)]
mod test;
