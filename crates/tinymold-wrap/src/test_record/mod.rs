//! The two records the test result cache keeps.
//!
//! A **test-binary record** (`tests/bin/<binary digest>`) says that the
//! wrapper built this executable from a `--test` compile, and names what the
//! binary can run or read from the build besides its own bytes: the dynamic
//! libraries in its directory it was linked against, the package binaries
//! cargo told it about (`CARGO_BIN_EXE_<name>`), and its build script's
//! `OUT_DIR`. The runner keys a run on all of them, and caches only
//! binaries with a record, which keeps `cargo run` targets and binaries
//! tinymold never saw out.
//!
//! A **test-run record** (`tests/run/<test key>`) says that a run with that
//! key passed, and names its stdout and stderr in the store, to replay. Only
//! passes are recorded, so the exit code is always 0 and not stored.
//!
//! Both are line-based text, like the compile records, and pinned in the
//! tests. See `docs/specs/test-cache.md`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use tinymold_hash::Digest;

use crate::{Error, Result};

const BINARY_HEADER: &str = "tinymold-test-binary 1";
const RUN_HEADER: &str = "tinymold-test-run 1";

/// What the wrapper knows about a test executable it built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestBinary {
    /// The crate the binary tests.
    pub(crate) crate_name: String,
    /// File names, in the binary's directory, of the dynamic libraries its
    /// compile linked against.
    pub(crate) dylibs: Vec<String>,
    /// Package binaries the test can run, as `${PROFILE}/…` or `${OUT}/…`
    /// paths.
    pub(crate) exes: Vec<String>,
    /// Build-script output directories the test can read, likewise.
    pub(crate) out_dirs: Vec<String>,
}

/// A recorded passing run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TestRun {
    /// The run's stdout.
    pub(crate) stdout: Digest,
    /// The run's stderr.
    pub(crate) stderr: Digest,
}

impl TestBinary {
    /// Where the record of the binary with content `digest` lives.
    pub(crate) fn path(store: &Path, digest: &Digest) -> PathBuf {
        store.join("tests").join("bin").join(digest.to_hex())
    }

    pub(crate) fn encode(&self) -> String {
        let mut text = format!("{BINARY_HEADER}\ncrate {}\n", self.crate_name);
        for dylib in &self.dylibs {
            let _ = writeln!(text, "dylib {dylib}");
        }
        for exe in &self.exes {
            let _ = writeln!(text, "exe {exe}");
        }
        for dir in &self.out_dirs {
            let _ = writeln!(text, "outdir {dir}");
        }
        text
    }

    pub(crate) fn decode(text: &str, path: &Path) -> Result<Self> {
        let corrupt = corrupt(path);
        let mut lines = text.lines();
        if lines.next() != Some(BINARY_HEADER) {
            return Err(corrupt(1));
        }
        let crate_name = lines
            .next()
            .and_then(|line| line.strip_prefix("crate "))
            .filter(|name| !name.is_empty())
            .ok_or_else(|| corrupt(2))?
            .to_owned();
        let mut binary = Self {
            crate_name,
            dylibs: Vec::new(),
            exes: Vec::new(),
            out_dirs: Vec::new(),
        };
        for (index, line) in lines.enumerate() {
            let (list, value) = if let Some(name) = line.strip_prefix("dylib ") {
                (&mut binary.dylibs, Some(name).filter(|name| is_file_name(name)))
            } else if let Some(path) = line.strip_prefix("exe ") {
                (&mut binary.exes, Some(path).filter(|path| is_build_path(path)))
            } else if let Some(path) = line.strip_prefix("outdir ") {
                (&mut binary.out_dirs, Some(path).filter(|path| is_build_path(path)))
            } else {
                return Err(corrupt(index + 3));
            };
            list.push(value.ok_or_else(|| corrupt(index + 3))?.to_owned());
        }
        Ok(binary)
    }
}

impl TestRun {
    /// Where the record of the run with test key `key` lives.
    pub(crate) fn path(store: &Path, key: &Digest) -> PathBuf {
        store.join("tests").join("run").join(key.to_hex())
    }

    pub(crate) fn encode(&self) -> String {
        format!(
            "{RUN_HEADER}\nstdout {}\nstderr {}\n",
            self.stdout, self.stderr
        )
    }

    /// Every object replaying this run reads.
    pub(crate) fn references(&self) -> Vec<Digest> {
        vec![self.stdout, self.stderr]
    }

    pub(crate) fn decode(text: &str, path: &Path) -> Result<Self> {
        let corrupt = corrupt(path);
        let mut lines = text.lines();
        if lines.next() != Some(RUN_HEADER) {
            return Err(corrupt(1));
        }
        let mut digest_line = |name: &str, line: usize| {
            lines
                .next()
                .and_then(|text| text.strip_prefix(name))
                .and_then(|value| Digest::from_str(value).ok())
                .ok_or_else(|| corrupt(line))
        };
        let stdout = digest_line("stdout ", 2)?;
        let stderr = digest_line("stderr ", 3)?;
        if lines.next().is_some() {
            return Err(corrupt(4));
        }
        Ok(Self { stdout, stderr })
    }
}

fn corrupt(path: &Path) -> impl Fn(usize) -> Error + '_ {
    move |line| Error::CorruptRecord {
        path: path.to_path_buf(),
        line,
    }
}

/// Whether `path` names something inside the build: a path under
/// `${PROFILE}` or `${OUT}`, without `..`, which the runner expands against
/// the test binary's own target directory.
pub(crate) fn is_build_path(path: &str) -> bool {
    let Some(rest) = path
        .strip_prefix("${PROFILE}/")
        .or_else(|| path.strip_prefix("${OUT}/"))
    else {
        return false;
    };
    !rest.is_empty() && rest.split(['/', '\\']).all(|part| !part.is_empty() && part != "..")
}

/// Whether `name` is one plain file name: a record must not name a file
/// outside the directory it is joined to.
fn is_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':'])
}

#[cfg(test)]
mod test;
