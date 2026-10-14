//! The two digests of a two-phase lookup.
//!
//! The **pre-key** covers everything known before rustc runs: the compiler,
//! the normalized arguments, the content of every `--extern` artifact and
//! native search directory, a few environment variables rustc reads on its
//! own, and the linker for units that link. The **full key** adds what the
//! compile actually read, which only an earlier compile's dep-info can say:
//! every input file's content and every `env!` variable's value.
//!
//! Absolute workspace paths are normalized away, so the same unit hits from
//! any worktree, with one exception: a compile instrumented for coverage
//! keeps its workspace path, because its coverage mappings embed it.
//!
//! Every field goes through [`Hasher::field`], which length-prefixes it, and
//! every list is preceded by its length, so no two different inputs encode to
//! the same bytes. The schema tag starts every key; changing anything here
//! means changing the tag, so old entries miss instead of being misread.

use tinymold_hash::{Digest, Hasher};

use crate::args::KeyArg;
use crate::{Invocation, Roots};

/// The schema tag of every pre-key.
// v2: actions record every file with the unit's prefix, including artifacts
// incremental rustc reuses with old mtimes, which v1 entries may lack.
const SCHEMA: &str = "tinymold/rustc/v2";

/// Environment variables rustc reads without recording them in dep-info.
pub const KEY_ENV: &[&str] = &[
    "RUSTC_BOOTSTRAP",
    "SOURCE_DATE_EPOCH",
    "MACOSX_DEPLOYMENT_TARGET",
    "IPHONEOS_DEPLOYMENT_TARGET",
    "TVOS_DEPLOYMENT_TARGET",
    "WATCHOS_DEPLOYMENT_TARGET",
    "SDKROOT",
];

/// Everything the pre-key covers, gathered by the caller.
#[derive(Debug, Clone)]
pub struct PreKeyInputs<'a> {
    /// The parsed invocation.
    pub invocation: &'a Invocation,
    /// Its roots, for normalizing paths in arguments.
    pub roots: &'a Roots,
    /// `rustc -vV` output.
    pub rustc_version: &'a str,
    /// The value of each [`KEY_ENV`] variable, `None` when unset.
    pub env: &'a [(String, Option<String>)],
    /// The digest of each `--extern` artifact, indexed like
    /// `invocation.externs`; `None` for an extern without a path.
    pub externs: &'a [Option<Digest>],
    /// The `(file name, digest)` of every file in each search directory,
    /// sorted by name, indexed like `invocation.search_paths`.
    pub search_paths: &'a [Vec<(String, Digest)>],
    /// The linker's identity, for units that link.
    pub linker: Option<&'a str>,
    /// The identity of a driver wrapping rustc, such as clippy-driver's
    /// version and configuration; `None` for rustc itself.
    pub driver: Option<&'a str>,
}

/// The pre-key: the digest of everything known before rustc runs.
#[must_use]
pub fn pre_key(inputs: &PreKeyInputs<'_>) -> Digest {
    let invocation = inputs.invocation;
    let roots = inputs.roots;
    let mut hasher = Hasher::new();
    hasher.field(SCHEMA.as_bytes());
    hasher.field(inputs.rustc_version.as_bytes());
    let source = invocation.source.to_string_lossy();
    hasher.field(roots.normalize(&source).as_bytes());

    hasher.field(&(invocation.key_args.len() as u64).to_le_bytes());
    for arg in &invocation.key_args {
        match arg {
            KeyArg::Text(text) => {
                hasher.field(b"t").field(roots.normalize(text).as_bytes());
            }
            KeyArg::Extern(index) => {
                hasher
                    .field(b"e")
                    .field(invocation.externs[*index].name.as_bytes());
                match inputs.externs.get(*index).copied().flatten() {
                    Some(digest) => hasher.digest(&digest),
                    None => hasher.field(b""),
                };
            }
            KeyArg::Search(index) => {
                let search = &invocation.search_paths[*index];
                let path = roots.normalize(&search.path.to_string_lossy());
                hasher
                    .field(b"L")
                    .field(search.kind.as_bytes())
                    .field(path.as_bytes());
                let files = inputs
                    .search_paths
                    .get(*index)
                    .map_or(&[][..], Vec::as_slice);
                hasher.field(&(files.len() as u64).to_le_bytes());
                for (name, digest) in files {
                    hasher.field(name.as_bytes()).digest(digest);
                }
            }
        }
    }

    hash_env(&mut hasher, inputs.env);
    hasher.field(inputs.linker.unwrap_or("").as_bytes());
    if let Some(driver) = inputs.driver {
        // Only a driver adds this field, so rustc's own keys are unchanged.
        hasher.field(b"driver").field(driver.as_bytes());
    }
    if invocation.instruments_coverage() {
        // Coverage mappings name source files by absolute path, so a hit from
        // another workspace would report coverage of that workspace's files.
        hasher
            .field(b"coverage-cwd")
            .field(roots.expand("${CWD}").as_bytes());
    }
    hasher.finish()
}

/// The full key: the pre-key plus every input file's normalized path and
/// content digest, and every env-dep's value, in dep-info order.
#[must_use]
pub fn full_key(
    pre_key: &Digest,
    inputs: &[(String, Digest)],
    env: &[(String, Option<String>)],
) -> Digest {
    let mut hasher = Hasher::new();
    hasher.field(b"full").digest(pre_key);
    hasher.field(&(inputs.len() as u64).to_le_bytes());
    for (path, digest) in inputs {
        hasher.field(path.as_bytes()).digest(digest);
    }
    hash_env(&mut hasher, env);
    hasher.finish()
}

fn hash_env(hasher: &mut Hasher, env: &[(String, Option<String>)]) {
    hasher.field(&(env.len() as u64).to_le_bytes());
    for (name, value) in env {
        hasher.field(name.as_bytes());
        match value {
            Some(value) => hasher.field(b"=").field(value.as_bytes()),
            None => hasher.field(b"!"),
        };
    }
}

#[cfg(test)]
mod test;
