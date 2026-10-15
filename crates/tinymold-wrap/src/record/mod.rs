//! The two records the cache keeps besides store objects.
//!
//! An **action** (`entries/<crate>/<full key>`) says what one compile
//! produced: the digest of each output file, whether it is executable, and
//! whether it is text stored with placeholders (dep-info), plus the digests of
//! rustc's stdout and stderr. It is written after every object it names is in
//! the store, so a reader that finds one can always restore it.
//!
//! An action of an incremental compile may also carry its incremental
//! session: a `session <name> <dir>` line after the outputs, then one
//! `sfile <digest> <name>` line per file in it.
//!
//! A **pre-key record** (`prekeys/<pre-key>`) lists the input sets earlier
//! compiles with this pre-key read: each input's normalized path, its
//! identity when recorded (device, inode, size, mtime) and its digest, plus the
//! names of the env vars it read. A lookup tries each set, newest first.
//!
//! Both are line-based text, like the freshness manifest, so they can be read
//! with `cat`. Their encodings are pinned in the tests.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

use tinymold_hash::Digest;

use crate::error::io;
use crate::{Error, Result};

/// How many input sets a pre-key record keeps; older ones are dropped.
pub(crate) const MAX_SETS: usize = 8;

/// One output of a compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Output {
    /// File name inside the out-dir.
    pub(crate) name: String,
    /// Digest of the stored object.
    pub(crate) digest: Digest,
    /// Whether the file is executable.
    pub(crate) executable: bool,
    /// Whether the object is text with root placeholders to expand.
    pub(crate) text: bool,
}

/// What one compile produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Action {
    /// rustc's stdout, with root placeholders.
    pub(crate) stdout: Digest,
    /// rustc's stderr, with root placeholders.
    pub(crate) stderr: Digest,
    /// Every output file.
    pub(crate) outputs: Vec<Output>,
    /// The incremental session rustc left for this compile, when it was
    /// incremental and the session could be found.
    pub(crate) session: Option<SessionRecord>,
}

/// An incremental session stored with an action: restored on a hit into the
/// directory it came from, so the next edit there compiles incrementally
/// (see `crate::session`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionRecord {
    /// The crate's incremental directory it was taken from, absolute:
    /// `<incremental dir>/<crate>-<id>`.
    pub(crate) dir: String,
    /// The session directory's name, `s-<time>-<random>-<hash>`.
    pub(crate) name: String,
    /// Every file in it, by name.
    pub(crate) files: Vec<(String, Digest)>,
}

/// One input file of a recorded set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Input {
    /// Normalized path, as the key sees it.
    pub(crate) path: String,
    /// The file's identity when recorded; a matching file reuses `digest`.
    pub(crate) identity: Identity,
    /// Content digest.
    pub(crate) digest: Digest,
}

/// What `stat` says about a file, enough to tell it has not changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Identity {
    /// Device number; 0 where the platform has none.
    pub(crate) dev: u64,
    /// Inode number; 0 where the platform has none, which disables reuse.
    pub(crate) ino: u64,
    /// Size in bytes.
    pub(crate) size: u64,
    /// Modification time in nanoseconds since the Unix epoch.
    pub(crate) mtime_ns: u64,
}

/// The inputs one earlier compile read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputSet {
    /// When the set was recorded, in nanoseconds since the Unix epoch.
    pub(crate) recorded_ns: u64,
    /// Every input file, in dep-info order.
    pub(crate) inputs: Vec<Input>,
    /// Every env var read, in dep-info order.
    pub(crate) env: Vec<String>,
}

const ACTION_HEADER: &str = "tinymold-action 1";
const PREKEY_HEADER: &str = "tinymold-prekey 1";

impl Identity {
    fn encode(self) -> String {
        format!("{} {} {} {}", self.dev, self.ino, self.size, self.mtime_ns)
    }
}

impl Action {
    pub(crate) fn encode(&self) -> String {
        let mut text = format!(
            "{ACTION_HEADER}\nstdout {}\nstderr {}\n",
            self.stdout, self.stderr
        );
        for output in &self.outputs {
            let _ = writeln!(
                text,
                "output {} {} {} {}",
                output.digest,
                u8::from(output.executable),
                u8::from(output.text),
                output.name
            );
        }
        if let Some(session) = &self.session {
            let _ = writeln!(text, "session {} {}", session.name, session.dir);
            for (name, digest) in &session.files {
                let _ = writeln!(text, "sfile {digest} {name}");
            }
        }
        text
    }

    /// Every object restoring this action reads, its session's included.
    pub(crate) fn references(&self) -> Vec<Digest> {
        let mut digests = vec![self.stdout, self.stderr];
        digests.extend(self.outputs.iter().map(|output| output.digest));
        if let Some(session) = &self.session {
            digests.extend(session.files.iter().map(|(_, digest)| *digest));
        }
        digests
    }

    pub(crate) fn decode(text: &str, path: &Path) -> Result<Self> {
        let corrupt = |line: usize| Error::CorruptRecord {
            path: path.to_path_buf(),
            line,
        };
        let mut lines = text.lines();
        if lines.next() != Some(ACTION_HEADER) {
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
        let mut outputs = Vec::new();
        let mut session: Option<SessionRecord> = None;
        for (index, line) in lines.enumerate() {
            let parsed = if let Some(rest) = line.strip_prefix("session ") {
                // One session, after every output.
                parse_session(rest)
                    .filter(|_| session.is_none())
                    .map(|record| session = Some(record))
            } else if let Some(rest) = line.strip_prefix("sfile ") {
                parse_named_digest(rest)
                    .zip(session.as_mut())
                    .map(|(file, session)| session.files.push(file))
            } else if session.is_none() {
                parse_output(line).map(|output| outputs.push(output))
            } else {
                None
            };
            parsed.ok_or_else(|| corrupt(index + 4))?;
        }
        Ok(Self {
            stdout,
            stderr,
            outputs,
            session,
        })
    }
}

fn parse_output(line: &str) -> Option<Output> {
    let mut parts = line.strip_prefix("output ")?.splitn(4, ' ');
    let digest = Digest::from_str(parts.next()?).ok()?;
    let executable = flag(parts.next()?)?;
    let text = flag(parts.next()?)?;
    let name = parts.next().filter(|name| is_file_name(name))?.to_owned();
    Some(Output {
        name,
        digest,
        executable,
        text,
    })
}

/// `<name> <dir>` of a `session` line: a session directory name and the
/// absolute directory it came from.
fn parse_session(text: &str) -> Option<SessionRecord> {
    let (name, dir) = text.split_once(' ')?;
    (is_file_name(name) && name.starts_with("s-") && Path::new(dir).is_absolute()).then(|| {
        SessionRecord {
            dir: dir.to_owned(),
            name: name.to_owned(),
            files: Vec::new(),
        }
    })
}

/// `<digest> <name>` of an `sfile` line.
fn parse_named_digest(text: &str) -> Option<(String, Digest)> {
    let (digest, name) = text.split_once(' ')?;
    let digest = Digest::from_str(digest).ok()?;
    is_file_name(name).then(|| (name.to_owned(), digest))
}

/// Whether `name` is one plain file name, which restoring it joins to the
/// output directory: a record from a remote must not name a path outside it.
fn is_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':'])
}

fn flag(text: &str) -> Option<bool> {
    match text {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// Encodes the input sets of one pre-key, newest first.
pub(crate) fn encode_sets(sets: &[InputSet]) -> String {
    let mut text = format!("{PREKEY_HEADER}\n");
    for set in sets {
        let _ = writeln!(text, "set {}", set.recorded_ns);
        for input in &set.inputs {
            let _ = writeln!(
                text,
                "input {} {} {}",
                input.identity.encode(),
                input.digest,
                input.path
            );
        }
        for name in &set.env {
            let _ = writeln!(text, "env {name}");
        }
    }
    text
}

/// Parses a pre-key record.
pub(crate) fn decode_sets(text: &str, path: &Path) -> Result<Vec<InputSet>> {
    let corrupt = |line: usize| Error::CorruptRecord {
        path: path.to_path_buf(),
        line,
    };
    let mut lines = text.lines().enumerate();
    if lines.next().map(|(_, line)| line) != Some(PREKEY_HEADER) {
        return Err(corrupt(1));
    }
    let mut sets: Vec<InputSet> = Vec::new();
    for (index, line) in lines {
        if let Some(recorded) = line.strip_prefix("set ") {
            let recorded_ns = recorded.parse().map_err(|_| corrupt(index + 1))?;
            sets.push(InputSet {
                recorded_ns,
                inputs: Vec::new(),
                env: Vec::new(),
            });
            continue;
        }
        let set = sets.last_mut().ok_or_else(|| corrupt(index + 1))?;
        if let Some(input) = line.strip_prefix("input ") {
            set.inputs
                .push(parse_input(input).ok_or_else(|| corrupt(index + 1))?);
        } else if let Some(name) = line.strip_prefix("env ").filter(|name| !name.is_empty()) {
            set.env.push(name.to_owned());
        } else {
            return Err(corrupt(index + 1));
        }
    }
    Ok(sets)
}

fn parse_input(text: &str) -> Option<Input> {
    let mut parts = text.splitn(6, ' ');
    let mut number = || parts.next()?.parse().ok();
    let identity = Identity {
        dev: number()?,
        ino: number()?,
        size: number()?,
        mtime_ns: number()?,
    };
    let digest = Digest::from_str(parts.next()?).ok()?;
    let path = parts.next().filter(|path| !path.is_empty())?.to_owned();
    Some(Input {
        path,
        identity,
        digest,
    })
}

/// Reads a record file, or `None` when it does not exist.
pub(crate) fn read(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io("read", path)(error)),
    }
}

/// Records that the record at `path` was just used, by setting its mtime to
/// now: the last-use time `tinymold gc` expires by. Best effort, since a
/// failure only makes the record look older than it is.
pub(crate) fn touch(path: &Path) {
    if let Ok(file) = fs::File::open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// Writes a record through a temporary file and a rename, so concurrent
/// readers see the old record or the new one, never half of either.
pub(crate) fn write_atomically(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(io("create", dir))?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("record"),
        std::process::id()
    ));
    fs::write(&temp, text).map_err(io("write", &temp))?;
    fs::rename(&temp, path).map_err(io("rename into place", path))
}

#[cfg(test)]
mod test;
