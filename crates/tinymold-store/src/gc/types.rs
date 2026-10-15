//! What a collection may remove, and what it did.

use std::time::{Duration, SystemTime};

/// How long an unreferenced object or a temporary file is left alone, in
/// case the upload that wrote it is still working: its objects are stored
/// before its entry, and would otherwise look unreferenced.
pub const GRACE: Duration = Duration::from_secs(3600);

/// What [`collect`](crate::collect) may remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Remove entries not used for this long. `None` keeps them whatever
    /// their age.
    pub max_age: Option<Duration>,
    /// Evict the least recently used entries while the objects they
    /// reference take more than this many bytes. `None` sets no budget.
    pub max_size: Option<u64>,
    /// Report what would be removed, but remove nothing.
    pub dry_run: bool,
    /// The time ages are measured from.
    pub now: SystemTime,
    /// How old an unreferenced object or temporary must be to be removed.
    pub grace: Duration,
}

impl Policy {
    /// A policy that removes only garbage: unreferenced objects and stale
    /// temporaries, measured from now with the standard [`GRACE`].
    #[must_use]
    pub fn garbage_only() -> Self {
        Self {
            max_age: None,
            max_size: None,
            dry_run: false,
            now: SystemTime::now(),
            grace: GRACE,
        }
    }

    /// Whether something last used at `when` is older than `age`.
    pub(crate) fn older_than(&self, when: SystemTime, age: Option<Duration>) -> bool {
        age.is_some_and(|age| {
            self.now
                .duration_since(when)
                .is_ok_and(|elapsed| elapsed > age)
        })
    }
}

/// What a collection removed and kept. In a dry run, what it would have.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    /// Entries removed: expired, evicted for the budget, or malformed.
    pub entries_removed: u64,
    /// Entries kept.
    pub entries_kept: u64,
    /// Objects removed because no kept entry referenced them.
    pub objects_removed: u64,
    /// The bytes those objects held.
    pub bytes_freed: u64,
    /// Objects kept.
    pub objects_kept: u64,
    /// The bytes kept objects hold.
    pub bytes_kept: u64,
    /// Stale temporaries and index files removed.
    pub stale_files_removed: u64,
}
