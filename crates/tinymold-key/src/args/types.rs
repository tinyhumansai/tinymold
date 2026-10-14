//! The parsed form of one rustc command line.

use std::path::PathBuf;

/// How [`Invocation::parse`](super::Invocation::parse) judged a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Classified {
    /// A crate compile the cache can key and restore.
    Cacheable(Box<Invocation>),
    /// Anything else, run unchanged. The reason is for remarks and logs.
    PassThrough(&'static str),
}

/// A cacheable rustc invocation: one crate, compiled into one out-dir.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Invocation {
    /// `--crate-name`.
    pub crate_name: String,
    /// Every `--crate-type`, in order; empty means rustc's default (`bin`).
    pub crate_types: Vec<String>,
    /// The `--emit` kinds, such as `dep-info`, `metadata` and `link`.
    pub emit: Vec<String>,
    /// `--out-dir`, where every output lands.
    pub out_dir: PathBuf,
    /// `-C extra-filename`, the suffix cargo puts on every output name.
    pub extra_filename: String,
    /// The crate root source file, as given (often relative to the current
    /// directory).
    pub source: PathBuf,
    /// Every `--extern`, in order.
    pub externs: Vec<Extern>,
    /// Every `-L` other than `dependency=`, in order.
    pub search_paths: Vec<SearchPath>,
    /// `-C linker=`, when given.
    pub linker: Option<PathBuf>,
    /// Whether `--test` was passed.
    pub test: bool,
    /// Every argument, in order, with the ones that only say where outputs go
    /// (`--out-dir`, `-C incremental=`, `-L dependency=`) removed and
    /// `--extern` and `-L` paths left for the key to replace by digests. Paths
    /// are not normalized yet.
    pub(crate) key_args: Vec<KeyArg>,
}

/// One `--extern` argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extern {
    /// The name the crate is known by in the dependent.
    pub name: String,
    /// The artifact (`.rlib`, `.rmeta` or proc-macro dylib), when one is
    /// given.
    pub path: Option<PathBuf>,
}

/// One `-L` search path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPath {
    /// `native`, `crate`, `framework`, `all`, or `all` when no kind is given.
    pub kind: String,
    /// The directory.
    pub path: PathBuf,
}

/// An argument as the key sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeyArg {
    /// Plain text; absolute paths in it are normalized by the key.
    Text(String),
    /// An `--extern` whose artifact the key replaces by its digest.
    Extern(usize),
    /// A `-L` whose directory the key replaces by its contents' digests.
    Search(usize),
}
