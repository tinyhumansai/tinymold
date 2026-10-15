//! The key of one test run, for replaying a recorded pass.
//!
//! A test's outcome depends on the code linked into its binary and on what
//! it reads when it runs. The binary's content digest covers the first
//! exactly: every logic change in its dependency closure changes its bytes
//! (`docs/adr/0008-test-results-keyed-by-binary-digest.md`). The rest of the
//! key covers the second as well as can be known without tracing the run:
//! the arguments, the host, the files under the package root, files the
//! package declares, and the environment.
//!
//! Like the rustc keys, every field goes through [`Hasher::field`], every
//! list is preceded by its length, and the schema tag starts the key. The
//! list of ignored environment variables is part of the encoding: changing
//! it means changing the tag. See `docs/specs/test-cache.md`.

use tinymold_hash::{Digest, Hasher};

use crate::Roots;

/// The schema tag of every test key.
const SCHEMA: &str = "tinymold/test/v1";

/// Environment variables that only describe the terminal, the shell or
/// tinymold itself, and so are left out of the key.
pub const TEST_ENV_IGNORED: &[&str] = &[
    "TERM",
    "COLORTERM",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "NO_COLOR",
    "COLUMNS",
    "LINES",
    "SHLVL",
    "_",
    "OLDPWD",
    "PWD",
    "CARGO_MAKEFLAGS",
    "MAKEFLAGS",
    "MFLAGS",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
];

/// Prefixes of environment variables left out of the key, for the same
/// reason as [`TEST_ENV_IGNORED`].
pub const TEST_ENV_IGNORED_PREFIXES: &[&str] = &["SSH_", "TINYMOLD_"];

/// Everything a test key covers, gathered by the caller.
#[derive(Debug, Clone)]
pub struct TestKeyInputs<'a> {
    /// The content digest of the test binary.
    pub binary: &'a Digest,
    /// The identity of a runner the binary is run through, such as an
    /// emulator, as the caller formats it (path and digest); `None` when the
    /// binary runs directly.
    pub via: Option<&'a str>,
    /// The arguments after the binary, in order.
    pub args: &'a [String],
    /// The roots of the run: the working directory (the package root), the
    /// binary's directory and `CARGO_HOME`, for normalizing paths in
    /// arguments and env values. The working directory itself is not keyed:
    /// normalized, it is always `${CWD}`, and its contents are the package
    /// tree.
    pub roots: &'a Roots,
    /// The host the test runs on: OS, architecture and kernel release.
    pub host: &'a str,
    /// The [`tree_digest`] of every file under the package root.
    pub package_tree: &'a Digest,
    /// The [`tree_digest`] of the files the package declares as inputs.
    pub declared_inputs: &'a Digest,
    /// The environment the test process receives, in any order.
    pub env: &'a [(String, String)],
}

/// The test key: the digest of everything a recorded pass is valid for.
#[must_use]
pub fn test_key(inputs: &TestKeyInputs<'_>) -> Digest {
    let roots = inputs.roots;
    let mut hasher = Hasher::new();
    hasher.field(SCHEMA.as_bytes()).digest(inputs.binary);
    match inputs.via {
        Some(via) => hasher.field(b"via").field(via.as_bytes()),
        None => hasher.field(b""),
    };

    // Every argument, `--color` too: the replayed output is printed with
    // whatever coloring the recorded run chose.
    hasher.field(&(inputs.args.len() as u64).to_le_bytes());
    for arg in inputs.args {
        hasher.field(roots.normalize(arg).as_bytes());
    }
    hasher.field(inputs.host.as_bytes());
    hasher
        .digest(inputs.package_tree)
        .digest(inputs.declared_inputs);

    let mut env: Vec<(&str, String)> = inputs
        .env
        .iter()
        .filter(|(name, _)| !is_ignored_env(name))
        .map(|(name, value)| (name.as_str(), roots.normalize(value)))
        .collect();
    env.sort();
    hasher.field(&(env.len() as u64).to_le_bytes());
    for (name, value) in env {
        hasher.field(name.as_bytes()).field(value.as_bytes());
    }
    hasher.finish()
}

/// The digest of a set of files, each named by its path and content digest.
/// The order they are listed in does not matter.
#[must_use]
pub fn tree_digest(files: &[(String, Digest)]) -> Digest {
    let mut files: Vec<&(String, Digest)> = files.iter().collect();
    files.sort();
    let mut hasher = Hasher::new();
    hasher
        .field(b"tree")
        .field(&(files.len() as u64).to_le_bytes());
    for (path, digest) in files {
        hasher.field(path.as_bytes()).digest(digest);
    }
    hasher.finish()
}

/// Whether an environment variable is left out of the test key.
#[must_use]
pub fn is_ignored_env(name: &str) -> bool {
    TEST_ENV_IGNORED.contains(&name)
        || TEST_ENV_IGNORED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
mod test;
