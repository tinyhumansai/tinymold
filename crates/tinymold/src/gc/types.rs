//! The limits `tinymold gc` takes and the report it gives.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use crate::Error;

/// A length of time written as a whole number and a unit: `90s`, `30m`,
/// `36h` or `14d`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Age(pub Duration);

impl FromStr for Age {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || Error::InvalidDuration(text.to_owned());
        let split = text
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(invalid)?;
        let (number, unit) = text.split_at(split);
        let number: u64 = number.parse().map_err(|_| invalid())?;
        let seconds = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86_400,
            _ => return Err(invalid()),
        };
        number
            .checked_mul(seconds)
            .map(|total| Self(Duration::from_secs(total)))
            .ok_or_else(invalid)
    }
}

/// A number of bytes, written plainly or with a `K`, `M`, `G` or `T` suffix
/// (powers of 1024), and shown in the largest unit that keeps it above 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct ByteSize(pub u64);

impl FromStr for ByteSize {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || Error::InvalidSize(text.to_owned());
        let split = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        let (number, unit) = text.split_at(split);
        let number: u64 = number.parse().map_err(|_| invalid())?;
        let shift = match unit {
            "" | "B" => 0,
            "K" => 10,
            "M" => 20,
            "G" => 30,
            "T" => 40,
            _ => return Err(invalid()),
        };
        number.checked_mul(1 << shift).map(Self).ok_or_else(invalid)
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
        if self.0 < 1024 {
            return write!(f, "{} B", self.0);
        }
        // Exact enough for a report: the value is shown to one decimal.
        #[allow(clippy::cast_precision_loss)]
        let mut value = self.0 as f64 / 1024.0;
        let mut unit = 0;
        while value >= 1024.0 && unit + 1 < UNITS.len() {
            value /= 1024.0;
            unit += 1;
        }
        write!(f, "{value:.1} {}", UNITS[unit])
    }
}

/// How many objects a store holds and how many bytes they take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreUsage {
    /// Objects in the store.
    pub objects: u64,
    /// Their total size.
    pub bytes: ByteSize,
}

impl fmt::Display for StoreUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} objects, {}", self.objects, self.bytes)
    }
}

/// What one collection removed and kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Report {
    /// Whether nothing was actually deleted.
    pub dry_run: bool,
    /// Actions and recorded test runs removed, because they expired, did
    /// not fit the size budget, or were corrupt.
    pub actions_removed: u64,
    /// Actions and recorded test runs kept.
    pub actions_kept: u64,
    /// Pre-key records removed.
    pub prekeys_removed: u64,
    /// Digest-cache files, test-binary records and stale temporaries
    /// removed.
    pub caches_removed: u64,
    /// Objects removed, and their bytes.
    pub objects_removed: StoreUsage,
    /// Objects kept, and their bytes.
    pub objects_kept: StoreUsage,
    /// Snapshots removed.
    pub snapshots_removed: u64,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verb = if self.dry_run {
            "would remove"
        } else {
            "removed"
        };
        writeln!(
            f,
            "gc: {verb} {} actions, {} pre-keys, {} cache files, {} snapshots, {}",
            self.actions_removed,
            self.prekeys_removed,
            self.caches_removed,
            self.snapshots_removed,
            self.objects_removed
        )?;
        write!(
            f,
            "gc: kept {} actions, {}",
            self.actions_kept, self.objects_kept
        )
    }
}
