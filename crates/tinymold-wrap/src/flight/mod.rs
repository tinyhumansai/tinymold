//! Compiling a unit once when several builds want it at the same time.
//!
//! A miss takes an exclusive lock on `locks/<key>` in the store before it
//! compiles, and keeps it until the compile is stored. Another wrapper that
//! misses on the same key finds the lock held, waits for it, and looks the
//! unit up again, usually finding the first one's result. This is Zig's
//! build-cache locking (`docs/research/zig.md`), done per file with `flock`
//! (`LockFileEx` on Windows) so that it works without a daemon.
//!
//! The lock only saves work. Anything that stops it from being taken (a
//! filesystem without locking, an unwritable store) leaves the compile
//! unlocked rather than failing it, and a holder that dies releases its lock
//! with its process.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tinymold_hash::Digest;

/// The first pause between attempts to take a held lock.
const FIRST_POLL: Duration = Duration::from_millis(5);

/// The longest pause between attempts: how late a waiter may notice that a
/// lock was released.
const LAST_POLL: Duration = Duration::from_millis(100);

/// An exclusive lock on one key, released when dropped.
#[derive(Debug)]
pub(crate) struct Flight {
    _file: File,
}

/// What [`claim`] got.
#[derive(Debug)]
pub(crate) enum Claim {
    /// The lock was free and is now held.
    Held(Flight),
    /// The lock was held by another wrapper, which released it after this
    /// long; it is now held here.
    Waited(Flight, Duration),
    /// No lock: it could not be taken, or was still held at the limit.
    Unlocked,
}

/// The lock file for `key` in the store at `store`.
pub(crate) fn lock_path(store: &Path, key: &Digest) -> PathBuf {
    store.join("locks").join(key.to_hex())
}

/// Takes the lock on `key`, waiting up to `limit` while another wrapper
/// holds it.
pub(crate) fn claim(store: &Path, key: &Digest, limit: Duration) -> Claim {
    let path = lock_path(store, key);
    let Ok(file) = open(&path) else {
        return Claim::Unlocked;
    };
    match file.try_lock() {
        Ok(()) => return Claim::Held(Flight { _file: file }),
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(_)) => return Claim::Unlocked,
    }
    let started = Instant::now();
    let mut pause = FIRST_POLL;
    while started.elapsed() < limit {
        std::thread::sleep(pause.min(limit.saturating_sub(started.elapsed())));
        match file.try_lock() {
            Ok(()) => return Claim::Waited(Flight { _file: file }, started.elapsed()),
            Err(TryLockError::WouldBlock) => pause = (pause * 2).min(LAST_POLL),
            Err(TryLockError::Error(_)) => return Claim::Unlocked,
        }
    }
    Claim::Unlocked
}

/// Whether a wrapper holds the lock file at `path`. A file that cannot be
/// opened counts as held, so that it is left alone, unless it is gone.
#[must_use]
pub fn lock_held(path: &Path) -> bool {
    match File::open(path) {
        Ok(file) => !matches!(file.try_lock(), Ok(())),
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

fn open(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(test)]
mod test;
