//! Remembering which output files a hit or miss left in an out-dir.
//!
//! A unit's outputs often survive from one build to the next. After an
//! upstream edit that leaves its metadata unchanged (early cutoff), a hit
//! restores the very files already in the out-dir, and most of an incremental
//! unit's objects carry over between compiles. Placing hundreds of objects
//! again costs more than the rest of the hit: about 0.3 ms per file, or
//! 150 ms for ripgrep's `rg`.
//!
//! So after every hit and every stored miss, the wrapper writes one record
//! per unit and out-dir naming each output's identity and digest. A later
//! hit keeps a file in place when the action wants the same digest under
//! that name and the file's identity still matches the record, under the
//! racily-clean rule of [`crate::stat`]. Anything else is replaced.
//!
//! Records live under `stat/`, beside the digest caches, so `tinymold gc`
//! treats them as caches. Their format:
//!
//! ```text
//! tinymold-placed 1
//! recorded <ns>
//! <dev> <ino> <size> <mtime_ns> <digest> <name>
//! ```

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tinymold_hash::Digest;

use crate::Result;
use crate::record::{Identity, read, write_atomically};
use crate::stat::{nanos, still_valid};

/// The first line of every record.
const HEADER: &str = "tinymold-placed 1";

/// The outputs one unit last left in one out-dir.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Placed {
    recorded_ns: u64,
    files: HashMap<String, (Identity, Digest)>,
}

impl Placed {
    /// Where the record of the unit `stem` placing outputs in `out_dir` lives.
    pub(crate) fn path(store: &Path, out_dir: &Path, stem: &str) -> PathBuf {
        let name = Digest::of_bytes(
            format!("placed:{}\0{stem}", out_dir.to_string_lossy()).as_bytes(),
        )
        .to_hex();
        store.join("stat").join(&name[..2]).join(&name[2..])
    }

    /// The record at `path`; empty when there is none or it does not parse,
    /// since an unusable record only means every output is placed again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when the record exists but
    /// cannot be read.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        Ok(read(path)?
            .as_deref()
            .and_then(Self::decode)
            .unwrap_or_default())
    }

    /// Whether the file `name`, whose identity is now `current`, is still the
    /// one recorded and holds `digest`.
    pub(crate) fn holds(&self, name: &str, current: Identity, digest: Digest) -> bool {
        self.files.get(name).is_some_and(|(recorded, held)| {
            *held == digest && still_valid(*recorded, self.recorded_ns, current)
        })
    }

    /// Records that the out-dir now holds `files`, each as `(name, identity,
    /// digest)`, as of now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when the record cannot be
    /// written.
    pub(crate) fn save(path: &Path, files: &[(String, Identity, Digest)]) -> Result<()> {
        let placed = Self {
            recorded_ns: nanos(SystemTime::now()),
            files: files
                .iter()
                .map(|(name, identity, digest)| (name.clone(), (*identity, *digest)))
                .collect(),
        };
        write_atomically(path, &placed.encode())
    }

    fn encode(&self) -> String {
        let mut names: Vec<&String> = self.files.keys().collect();
        names.sort();
        let mut text = format!("{HEADER}\nrecorded {}\n", self.recorded_ns);
        for name in names {
            let (identity, digest) = &self.files[name];
            let _ = writeln!(
                text,
                "{} {} {} {} {digest} {name}",
                identity.dev, identity.ino, identity.size, identity.mtime_ns
            );
        }
        text
    }

    fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let recorded_ns = lines.next()?.strip_prefix("recorded ")?.parse().ok()?;
        let mut files = HashMap::new();
        for line in lines {
            let mut parts = line.splitn(6, ' ');
            let mut number = || parts.next()?.parse::<u64>().ok();
            let identity = Identity {
                dev: number()?,
                ino: number()?,
                size: number()?,
                mtime_ns: number()?,
            };
            let digest = parts.next()?.parse().ok()?;
            let name = parts.next().filter(|name| !name.is_empty())?;
            files.insert(name.to_owned(), (identity, digest));
        }
        Some(Self { recorded_ns, files })
    }
}

#[cfg(test)]
mod test;
