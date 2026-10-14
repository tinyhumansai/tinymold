//! The freshness manifest and its on-disk encoding.
//!
//! The manifest is a small text file, one workspace file per line, so it can
//! be read with `cat` when a restore does something surprising:
//!
//! ```text
//! tinymold-fresh 1
//! recorded 1760450000123456789
//! stamp 1760449990000000000
//! 1760440000000000000 42 <64 hex digits> src/main.rs
//! ```
//!
//! `recorded` is when the manifest was written and `stamp` is the newest
//! output mtime in the target directory at that moment, both in nanoseconds
//! since the Unix epoch. Each entry is the file's mtime, size, BLAKE3 digest
//! and path relative to the workspace root with `/` separators. Entries are
//! sorted by path so the same tree always encodes to the same bytes.

use std::fmt::Write as _;
use std::path::Path;
use std::str::FromStr;

use tinymold_hash::Digest;

use crate::{Error, Result};

/// The first line of every manifest; bump the number on any format change.
const HEADER: &str = "tinymold-fresh 1";

/// One workspace file as it was when the manifest was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Path relative to the workspace root, `/`-separated.
    pub(crate) path: String,
    /// Size in bytes.
    pub(crate) size: u64,
    /// Modification time in nanoseconds since the Unix epoch.
    pub(crate) mtime_ns: u64,
    /// BLAKE3 digest of the contents.
    pub(crate) digest: Digest,
}

/// A recorded workspace state, valid for one state of the target directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Manifest {
    /// When the manifest was written, in nanoseconds since the Unix epoch.
    pub(crate) recorded_ns: u64,
    /// The newest output mtime in the target directory when it was written.
    pub(crate) stamp_ns: u64,
    /// Every recorded file, sorted by path.
    pub(crate) entries: Vec<Entry>,
}

impl Manifest {
    /// Encodes the manifest in its canonical text form.
    pub(crate) fn encode(&self) -> String {
        let mut text = format!(
            "{HEADER}\nrecorded {}\nstamp {}\n",
            self.recorded_ns, self.stamp_ns
        );
        for entry in &self.entries {
            // Writing to a String cannot fail.
            let _ = writeln!(
                text,
                "{} {} {} {}",
                entry.mtime_ns, entry.size, entry.digest, entry.path
            );
        }
        text
    }

    /// Parses a manifest read from `path`; `path` only names the file in
    /// errors.
    pub(crate) fn decode(text: &str, path: &Path) -> Result<Self> {
        let corrupt = |line: usize| Error::CorruptManifest {
            path: path.to_path_buf(),
            line,
        };
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err(corrupt(1));
        }
        let mut field = |name: &str, line: usize| -> Result<u64> {
            lines
                .next()
                .and_then(|text| text.strip_prefix(name))
                .and_then(|value| value.strip_prefix(' '))
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| corrupt(line))
        };
        let recorded_ns = field("recorded", 2)?;
        let stamp_ns = field("stamp", 3)?;
        let entries = lines
            .enumerate()
            .map(|(index, line)| parse_entry(line).ok_or_else(|| corrupt(index + 4)))
            .collect::<Result<_>>()?;
        Ok(Self {
            recorded_ns,
            stamp_ns,
            entries,
        })
    }
}

fn parse_entry(line: &str) -> Option<Entry> {
    let mut parts = line.splitn(4, ' ');
    let mtime_ns = parts.next()?.parse().ok()?;
    let size = parts.next()?.parse().ok()?;
    let digest = Digest::from_str(parts.next()?).ok()?;
    let path = parts.next().filter(|path| !path.is_empty())?.to_owned();
    Some(Entry {
        path,
        size,
        mtime_ns,
        digest,
    })
}

/// What [`record`](super::record) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecordReport {
    /// Files written to the manifest.
    pub files: usize,
    /// Files that had to be hashed; the rest reused the previous manifest's
    /// digest because their size and mtime had not changed.
    pub hashed: usize,
    /// Files left out because their path cannot be encoded (not UTF-8, or it
    /// contains a newline).
    pub skipped: usize,
}

/// Per-file results of a [`restore`](super::restore) that ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct RestoreReport {
    /// Files whose content matched the manifest and whose mtime was put back.
    pub restored: usize,
    /// Files whose size and mtime already matched.
    pub unchanged: usize,
    /// Files whose content differs; they keep their mtime and cargo rebuilds
    /// what depends on them.
    pub changed: usize,
    /// Recorded files that no longer exist.
    pub missing: usize,
}

/// The result of [`restore`](super::restore).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Restore {
    /// The manifest applied; per-file results inside.
    Applied(RestoreReport),
    /// The target directory has no manifest, so nothing was restored.
    NoManifest,
    /// Something was built since the manifest was recorded, so its mtimes no
    /// longer describe the outputs and nothing was restored.
    TargetChanged,
}
