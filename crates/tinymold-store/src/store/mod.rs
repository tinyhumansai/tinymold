//! The store: entries by key, objects by digest, and the SHA-256 index.
//!
//! Layout under the store root, beside the content-addressed store's own
//! directories:
//!
//! ```text
//! <root>/
//! ├── objects/, tmp/        # tinymold-cas: objects by BLAKE3, its staging
//! ├── ac/ab/cdef…           # one encoded entry per key, sharded like objects
//! ├── sha256/ab/cdef…       # the 32-byte BLAKE3 name of the object with this SHA-256
//! └── pending/              # private files renamed into ac/ and sha256/
//! ```
//!
//! Every file is written privately under `pending/` and renamed into place,
//! so a reader sees a complete file or none, and several processes can share
//! one root without a lock. An entry is written only after every object it
//! names is committed.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tinymold_hash::Digest;

mod types;

pub use types::{Claim, Ingested};

use crate::disk::{io_error, read_if_present};
use crate::sha256::Sha256Reader;
use crate::{Entry, Error, Key, Result, Sha256};

#[cfg(doc)]
use crate::Namespace;

/// A cache store rooted at one directory: the objects of a
/// [`tinymold_cas::Store`] plus the entries that name them.
///
/// Opening a store is cheap and holds no locks or file handles.
#[derive(Debug, Clone)]
pub struct Store {
    cas: tinymold_cas::Store,
    root: PathBuf,
}

impl Store {
    /// Opens the store at `root`, creating its directories if needed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cas`] or [`Error::Io`] when the directories cannot
    /// be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let cas = tinymold_cas::Store::open(&root)?;
        for dir in ["ac", "sha256", "pending"] {
            let dir = root.join(dir);
            fs::create_dir_all(&dir).map_err(io_error("create", &dir))?;
        }
        Ok(Self { cas, root })
    }

    /// The directory this store lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The content-addressed store holding this store's objects.
    #[must_use]
    pub fn cas(&self) -> &tinymold_cas::Store {
        &self.cas
    }

    /// Streams `reader` into the store and names it by BLAKE3 and SHA-256.
    ///
    /// With a `claim`, the body is stored only if it hashes to the claimed
    /// digest; otherwise nothing is kept. Either way the body is read once
    /// and never held in memory as a whole.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DigestMismatch`] when the body does not match
    /// `claim`, [`Error::Cas`] when it cannot be read or stored, and
    /// [`Error::Io`] when the SHA-256 index cannot be written.
    pub fn ingest(&self, reader: impl Read, claim: Option<Claim>) -> Result<Ingested> {
        let mut hashing = Sha256Reader::new(reader);
        let staged = self.cas.stage(&mut hashing)?;
        let sha256 = hashing.finish();

        if let Some(claimed) = claim {
            let actual = match claimed {
                Claim::Blake3(_) => Claim::Blake3(staged.digest()),
                Claim::Sha256(_) => Claim::Sha256(sha256),
            };
            if actual != claimed {
                // Dropping `staged` discards the body.
                return Err(Error::DigestMismatch { claimed, actual });
            }
        }

        let size = staged.size();
        let digest = staged.commit()?;
        let index = self.sha256_path(&sha256);
        if !index.is_file() {
            self.write_atomic(&index, digest.as_bytes())?;
        }
        Ok(Ingested {
            digest,
            sha256,
            size,
        })
    }

    /// The stored object whose SHA-256 is `sha256`, if the store holds one.
    ///
    /// An index file whose object has since been removed, or that cannot be
    /// read as a digest, is a miss.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the index file exists but cannot be read.
    pub fn find_sha256(&self, sha256: &Sha256) -> Result<Option<Digest>> {
        let path = self.sha256_path(sha256);
        let Some(bytes) = read_if_present(&path)? else {
            return Ok(None);
        };
        let Ok(raw) = <[u8; Digest::LEN]>::try_from(bytes.as_slice()) else {
            return Ok(None);
        };
        let digest = Digest::from_bytes(raw);
        Ok(self.cas.contains(&digest).then_some(digest))
    }

    /// Stores `entry` under `key`, replacing any entry already there.
    ///
    /// Every object the entry names must already be in the store. Front-ends
    /// whose protocol forbids overwriting check [`Store::contains`] first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MissingObject`] for the first object the store does
    /// not hold, [`Error::MetaTooLarge`] when the entry cannot be encoded,
    /// and [`Error::Io`] when it cannot be written.
    pub fn put(&self, key: &Key, entry: &Entry) -> Result<()> {
        for blob in &entry.blobs {
            // `keep` also refreshes the object's mtime, which holds it inside
            // the collector's grace period while the entry is written.
            if !self.cas.keep(blob) {
                return Err(Error::MissingObject(*blob));
            }
        }
        let encoded = entry.encode()?;
        self.write_atomic(&self.entry_path(key), &encoded)
    }

    /// The entry stored under `key`, if any. A read counts as a use of the
    /// entry and of every object it names.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedEntry`] when the stored bytes do not decode,
    /// and [`Error::Io`] when they cannot be read.
    pub fn get(&self, key: &Key) -> Result<Option<Entry>> {
        let path = self.entry_path(key);
        let Some(bytes) = read_if_present(&path)? else {
            return Ok(None);
        };
        let entry = Entry::decode(&bytes)?;
        // A read is a use: refresh the entry's mtime, and each object's, so
        // the collector evicts least recently *used* first.
        touch(&path);
        for blob in &entry.blobs {
            // An object already gone makes the caller's read of it miss,
            // which is the safe outcome; nothing to do here.
            let _ = self.cas.keep(blob);
        }
        Ok(Some(entry))
    }

    /// Whether an entry is stored under `key`.
    #[must_use]
    pub fn contains(&self, key: &Key) -> bool {
        self.entry_path(key).is_file()
    }

    /// Where the entry for `key` lives, whether or not it exists.
    ///
    /// Keys come from [`Namespace::key`].
    #[must_use]
    pub fn entry_path(&self, key: &Key) -> PathBuf {
        sharded(&self.root.join("ac"), &key.to_string())
    }

    pub(crate) fn sha256_path(&self, sha256: &Sha256) -> PathBuf {
        sharded(&self.root.join("sha256"), &sha256.to_hex())
    }

    /// Writes `bytes` to a private file and renames it to `path`.
    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pending = self
            .root
            .join("pending")
            .join(format!("{}-{sequence}.tmp", std::process::id()));

        let written = fs::File::create_new(&pending)
            .and_then(|mut file| file.write_all(bytes))
            .map_err(io_error("write", &pending))
            .and_then(|()| {
                let shard = path.parent().unwrap_or(&self.root);
                fs::create_dir_all(shard).map_err(io_error("create", shard))
            })
            .and_then(|()| fs::rename(&pending, path).map_err(io_error("rename into place", path)));
        if written.is_err() {
            // Best effort: a stale pending file is garbage, not a fault.
            let _ = fs::remove_file(&pending);
        }
        written
    }
}

fn sharded(dir: &Path, hex: &str) -> PathBuf {
    dir.join(&hex[..2]).join(&hex[2..])
}

/// Sets an entry file's mtime to now, best effort: a missed refresh only
/// makes the entry look older to the collector.
fn touch(path: &Path) {
    if let Ok(file) = fs::File::options().write(true).open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

#[cfg(test)]
mod test;
