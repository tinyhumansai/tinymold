//! What `check` and `push` report.

use std::fmt;

/// What a server said it supports, from `GetCapabilities`, and whether this
/// client may write to it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// The digest functions the cache accepts, such as `sha256` and
    /// `blake3`. tinymold needs `sha256`.
    pub digest_functions: Vec<&'static str>,
    /// Whether clients may write `ActionCache` entries.
    pub updates_enabled: bool,
    /// Whether this client's credential (or lack of one) could upload a
    /// probe blob, which is what `push` needs.
    pub writable: bool,
    /// Whether a client with no credential could upload a probe blob, so
    /// that anyone who reaches the server can write to it.
    pub anonymous_writes: bool,
}

impl Capabilities {
    /// Whether the server names blobs by SHA-256, as tinymold needs.
    #[must_use]
    pub fn sha256(&self) -> bool {
        self.digest_functions.contains(&"sha256")
    }
}

impl fmt::Display for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let yes = |flag: bool| if flag { "yes" } else { "no" };
        write!(
            f,
            "digests {}, action cache updates {}, writable {}, anonymous writes {}",
            self.digest_functions.join(" "),
            yes(self.updates_enabled),
            yes(self.writable),
            yes(self.anonymous_writes)
        )
    }
}

/// What one `push` uploaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PushReport {
    /// Actions written to the `ActionCache`.
    pub actions: u64,
    /// Pre-key records written to the `ActionCache`.
    pub prekeys: u64,
    /// Actions skipped because an object they name is no longer in the local
    /// store.
    pub skipped: u64,
    /// Actions and pre-key records not written because an upload they need
    /// still failed after its retries. The rest of the push goes on without
    /// them.
    pub failed: u64,
    /// Blobs the server lacked and was sent.
    pub blobs: u64,
    /// Their total size in bytes.
    pub bytes: u64,
}

impl fmt::Display for PushReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pushed {} actions and {} pre-keys ({} new blobs, {} bytes); skipped {} incomplete actions",
            self.actions, self.prekeys, self.blobs, self.bytes, self.skipped
        )?;
        if self.failed > 0 {
            write!(
                f,
                "; {} entries not pushed after upload failures",
                self.failed
            )?;
        }
        Ok(())
    }
}
