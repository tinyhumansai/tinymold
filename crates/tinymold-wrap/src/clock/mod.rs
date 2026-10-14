//! Telling whether a file's modification time is safely in the past.
//!
//! A digest remembered next to a file's identity (size, inode, mtime) is only
//! sound if any later write must change that identity. A write within the same
//! timestamp tick keeps the mtime, so a file modified at or after the moment it
//! was hashed, give or take one tick, may have changed unseen: git's
//! racily-clean rule. Two things make "the moment it was hashed" subtle:
//!
//! - **Whose clock.** Filesystems stamp mtimes from their own clock, not the
//!   process's wall clock. Linux uses a coarse kernel clock that can lag the
//!   wall clock by a scheduler tick, and a network filesystem uses the
//!   server's. [`filesystem_now`] therefore reads "now" back as the mtime of a
//!   file it has just written, as Zig's build cache does.
//! - **How long a tick is.** HFS+ and ext3 keep whole seconds, FAT even
//!   seconds, exFAT hundredths. [`written_before`] infers the tick from the
//!   timestamp itself: a coarse filesystem leaves trailing decimal zeros, and
//!   a fine one leaves them only by chance, at the cost of a rare re-hash.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Result;
use crate::error::io;

/// Nanoseconds in a second.
const SECOND_NS: u64 = 1_000_000_000;

/// The coarsest tick in common use: FAT records even seconds.
const COARSEST_TICK_NS: u64 = 2 * SECOND_NS;

/// Nanoseconds since the Unix epoch; 0 before it, saturating far after it.
pub(crate) fn nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
    })
}

/// The time the filesystem holding `dir` gives a file written now, in
/// nanoseconds since the Unix epoch.
///
/// Writes a uniquely named file in `dir`, reads its mtime through the open
/// handle, and removes it. Compare file mtimes against this rather than the
/// wall clock: it is in the filesystem's own clock and granularity.
///
/// # Errors
///
/// Returns [`Error::Io`](crate::Error::Io) when `dir` cannot be created or
/// the file cannot be written or read back.
pub fn filesystem_now(dir: &Path) -> Result<u64> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(dir).map_err(io("create", dir))?;
    let probe = dir.join(format!(
        ".clock.{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let read_back = (|| {
        let mut file = File::create(&probe)?;
        file.write_all(b"now")?;
        file.metadata()?.modified()
    })();
    let _ = fs::remove_file(&probe);
    Ok(nanos(read_back.map_err(io("write", &probe))?))
}

/// The longest tick a filesystem could have used to produce `mtime_ns`:
/// ten to the power of its trailing decimal zeros, and two seconds for a
/// whole second, since FAT keeps only even ones.
pub(crate) fn tick_of(mtime_ns: u64) -> u64 {
    let mut tick = 1;
    while tick < SECOND_NS && mtime_ns.is_multiple_of(tick * 10) {
        tick *= 10;
    }
    if tick == SECOND_NS {
        COARSEST_TICK_NS
    } else {
        tick
    }
}

/// Whether every write that could have left a file with mtime `mtime_ns`
/// happened before `instant_ns`, both read from the same filesystem clock.
///
/// When it did, any later write gives the file a different mtime. When it
/// may not have, a later write within the same tick could leave the mtime
/// unchanged, so the file must be treated as possibly changed.
#[must_use]
pub fn written_before(mtime_ns: u64, instant_ns: u64) -> bool {
    mtime_ns.saturating_add(tick_of(mtime_ns)) <= instant_ns
}

#[cfg(test)]
mod test;
