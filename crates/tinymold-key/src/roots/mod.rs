//! Absolute paths that differ between worktrees, and their placeholders.
//!
//! The same unit built in two worktrees sees different absolute paths: the
//! current directory, the out-dir (`target/debug/deps`), the profile
//! directory above it (`target/debug`, where build-script outputs live), and
//! sometimes `CARGO_HOME`. [`Roots::normalize`] replaces those prefixes with
//! `${OUT}`, `${PROFILE}`, `${CWD}` and `${CARGO_HOME}` so that keys and
//! stored text agree across worktrees, and [`Roots::expand`] puts the current
//! worktree's paths back when stored text is restored.

use std::path::{Path, PathBuf};

/// The placeholder for each root, in the order prefixes are tried: the
/// longest path first, so that `target/debug/deps` is not half-replaced as
/// `target/debug`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pairs: Vec<(String, &'static str)>,
}

impl Roots {
    /// The roots of one invocation: its current directory, its out-dir (made
    /// absolute against `cwd`), the profile directory derived from the
    /// out-dir, and `CARGO_HOME` when known. Paths that are not UTF-8 are left
    /// out, which only means they are not normalized.
    #[must_use]
    pub fn new(cwd: &Path, out_dir: &Path, cargo_home: Option<&Path>) -> Self {
        let out_dir = cwd.join(out_dir);
        let mut roots: Vec<(PathBuf, &'static str)> = vec![(cwd.to_path_buf(), "${CWD}")];
        if let Some(profile) = profile_dir(&out_dir) {
            roots.push((profile, "${PROFILE}"));
        }
        roots.push((out_dir, "${OUT}"));
        if let Some(home) = cargo_home {
            roots.push((home.to_path_buf(), "${CARGO_HOME}"));
        }
        let mut pairs: Vec<(String, &'static str)> = roots
            .into_iter()
            .filter_map(|(path, name)| {
                let text = path.to_str()?.trim_end_matches('/').to_owned();
                (!text.is_empty()).then_some((text, name))
            })
            .collect();
        pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.1.cmp(b.1)));
        Self { pairs }
    }

    /// `text` with every root replaced by its placeholder.
    #[must_use]
    pub fn normalize(&self, text: &str) -> String {
        self.pairs
            .iter()
            .fold(text.to_owned(), |text, (path, name)| {
                text.replace(path.as_str(), name)
            })
    }

    /// `text` with every placeholder replaced by this worktree's path; the
    /// inverse of [`normalize`](Self::normalize).
    #[must_use]
    pub fn expand(&self, text: &str) -> String {
        self.pairs
            .iter()
            .rev()
            .fold(text.to_owned(), |text, (path, name)| {
                text.replace(name, path)
            })
    }
}

/// The cargo profile directory above an out-dir: the parent of `deps` or
/// `examples`, or the grandparent of a build-script directory
/// (`<profile>/build/<pkg>-<hash>`).
fn profile_dir(out_dir: &Path) -> Option<PathBuf> {
    let parent = out_dir.parent()?;
    match out_dir.file_name()?.to_str()? {
        "deps" | "examples" => Some(parent.to_path_buf()),
        _ if parent.file_name()? == "build" => Some(parent.parent()?.to_path_buf()),
        _ => None,
    }
}

#[cfg(test)]
mod test;
