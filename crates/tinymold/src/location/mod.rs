//! Where the shared store lives when no `--store` is given.
//!
//! One store per user, shared by every project, worktree, and agent on the
//! machine, is what lets identical artifacts be kept once. The location is
//! resolved from the environment in this order:
//!
//! 1. `TINYMOLD_STORE`, used as-is;
//! 2. `XDG_CACHE_HOME/tinymold`;
//! 3. `LOCALAPPDATA/tinymold` (Windows);
//! 4. `HOME/.cache/tinymold`.
//!
//! The first variable that is set and non-empty wins.

use std::ffi::OsString;
use std::path::PathBuf;

/// Resolves the default store directory using `lookup` to read environment
/// variables, or `None` when none of them is set.
///
/// Taking the lookup as a function keeps this testable without mutating the
/// process environment:
///
/// ```
/// # use std::path::PathBuf;
/// let env = |name: &str| (name == "HOME").then(|| "/home/ferris".into());
/// assert_eq!(
///     tinymold::default_store(env),
///     Some(PathBuf::from("/home/ferris/.cache/tinymold"))
/// );
/// ```
#[must_use]
pub fn default_store(lookup: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |name: &str| lookup(name).filter(|value| !value.is_empty());

    if let Some(store) = set("TINYMOLD_STORE") {
        return Some(PathBuf::from(store));
    }
    if let Some(cache) = set("XDG_CACHE_HOME").or_else(|| set("LOCALAPPDATA")) {
        return Some(PathBuf::from(cache).join("tinymold"));
    }
    set("HOME").map(|home| PathBuf::from(home).join(".cache").join("tinymold"))
}

#[cfg(test)]
mod test;
