//! What `check` and `push` report.

use std::fmt;

/// What a server said it supports, from `GetCapabilities`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// Whether it names blobs by SHA-256, which tinymold needs.
    pub sha256: bool,
    /// Whether it also accepts BLAKE3 digests.
    pub blake3: bool,
    /// Whether clients may write `ActionCache` entries.
    pub updates_enabled: bool,
}

impl fmt::Display for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let yes = |flag: bool| if flag { "yes" } else { "no" };
        write!(
            f,
            "sha256 {}, blake3 {}, action cache updates {}",
            yes(self.sha256),
            yes(self.blake3),
            yes(self.updates_enabled)
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
        )
    }
}
