//! What the catalog reports about each file the cache keeps.

use std::path::PathBuf;
use std::time::SystemTime;

use tinymold_hash::Digest;

/// What a cache file is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordKind {
    /// An action, `entries/<crate>/<full key>`: what one compile produced.
    Action {
        /// The crate it compiled.
        crate_name: String,
        /// Its full key.
        key: Digest,
    },
    /// A pre-key record, `prekeys/<pre-key>`: the input sets earlier
    /// compiles with this pre-key read.
    PreKey {
        /// The pre-key.
        key: Digest,
    },
    /// A test-run record, `tests/run/<test key>`: a passing test run to
    /// replay. Like an action, it needs the objects it names.
    TestRun {
        /// Its test key.
        key: Digest,
    },
    /// A test-binary record, `tests/bin/<binary digest>`: a test executable
    /// the wrapper built. Losing one only stops that binary's runs from
    /// being cached until it is built or restored again.
    TestBinary {
        /// The binary's content digest.
        digest: Digest,
    },
    /// A digest-cache file under `stat/` or `rustc/`. Losing one costs a
    /// rehash, never a wrong hit.
    Cache,
    /// A record being written, or left behind by a writer that died.
    Temporary,
    /// A lock file, `locks/<key>`, that a miss holds while it compiles so
    /// that other builds wait for it ([`lock_held`](crate::lock_held) says
    /// whether one is held). Losing one that is not held costs nothing.
    Lock,
}

/// One file the cache keeps beside the store's objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordFile {
    /// What the file is.
    pub kind: RecordKind,
    /// Where it is.
    pub path: PathBuf,
    /// Its size in bytes.
    pub size: u64,
    /// Its mtime. For actions and pre-key records this is the last use: the
    /// time they were written or last served a hit.
    pub last_used: SystemTime,
}

/// What one action needs and which compile it records.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ActionInfo {
    /// Every object the action needs: its outputs and its replayed text.
    pub references: Vec<Digest>,
    /// The name of its dep-info output, such as `serde-1a2b3c4d5e6f7a8b.d`,
    /// which names the cargo unit it compiled: successive compiles of one
    /// unit, edit after edit, share it. `None` for an action without one.
    pub unit: Option<String>,
}
