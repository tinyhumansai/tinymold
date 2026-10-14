//! The on-disk object store.
//!
//! Layout under the store root:
//!
//! ```text
//! <root>/
//! ├── objects/
//! │   └── af/1349b9f5…   # one immutable, read-only file per digest,
//! │                      # sharded by the first byte to keep directories small
//! └── tmp/               # private staging files, renamed into objects/
//! ```
//!
//! Every insert writes a private staging file, hashes *that* file, and
//! renames it into place. Hashing the private copy means a process still
//! writing the source cannot make an object's name lie about its contents,
//! and the rename makes the object appear atomically: readers see either no
//! object or a complete one, never a partial write. Two processes inserting
//! the same bytes at once both succeed, and the store ends up with one object.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tinymold_hash::Digest;

use crate::materialize::{self, Method, Strategy};

use crate::{Error, Result};

/// A local content-addressed store rooted at one directory.
///
/// Opening a store is cheap and holds no locks or file handles, so many
/// processes — every worktree and agent on a machine — can open the same root
/// and share it.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Opens the store at `root`, creating its directories if needed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the store directories cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let store = Self { root: root.into() };
        for dir in [store.objects_dir(), store.tmp_dir()] {
            fs::create_dir_all(&dir).map_err(io_error("create", &dir))?;
        }
        Ok(store)
    }

    /// The directory this store lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the object named `digest` lives, whether or not it exists yet.
    #[must_use]
    pub fn object_path(&self, digest: &Digest) -> PathBuf {
        let hex = digest.to_hex();
        self.objects_dir().join(&hex[..2]).join(&hex[2..])
    }

    /// Whether an object named `digest` is in the store.
    #[must_use]
    pub fn contains(&self, digest: &Digest) -> bool {
        self.object_path(digest).is_file()
    }

    /// Stores `bytes` and returns their digest.
    ///
    /// Inserting bytes the store already holds is a cheap no-op.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the object cannot be written.
    pub fn insert_bytes(&self, bytes: &[u8]) -> Result<Digest> {
        let digest = Digest::of_bytes(bytes);
        if self.contains(&digest) {
            return Ok(digest);
        }

        let staging = self.staging_path();
        let mut file = create_new(&staging)?;
        file.write_all(bytes).map_err(io_error("write", &staging))?;
        drop(file);
        self.commit(&staging, digest)?;
        Ok(digest)
    }

    /// Stores a copy of the file at `path` and returns its digest.
    ///
    /// The source is cloned when the filesystem supports it, copied
    /// otherwise, and never hardlinked: a hardlink would let later writes to
    /// the source silently change a stored object.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] or [`Error::Unsupported`] when the source cannot
    /// be read or the object cannot be written, and [`Error::Hash`] when the
    /// staged copy cannot be hashed.
    pub fn insert_file(&self, path: impl AsRef<Path>) -> Result<Digest> {
        let source = path.as_ref();
        if !source.is_file() {
            return Err(Error::Io {
                action: "read",
                path: source.to_path_buf(),
                source: io::Error::from(io::ErrorKind::NotFound),
            });
        }

        let staging = self.staging_path();
        let staged = materialize::place_private_copy(source, &staging)
            .and_then(|_| Ok(Digest::of_file(&staging)?));

        match staged {
            Ok(digest) => {
                self.commit(&staging, digest)?;
                Ok(digest)
            }
            Err(error) => {
                // Best effort: a stale staging file is garbage, not a fault.
                let _ = fs::remove_file(&staging);
                Err(error)
            }
        }
    }

    /// Reads the whole object named `digest` into memory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] when the object is absent, and
    /// [`Error::Io`] when it cannot be read.
    pub fn read(&self, digest: &Digest) -> Result<Vec<u8>> {
        let path = self.existing_object(digest)?;
        fs::read(&path).map_err(io_error("read", &path))
    }

    /// Re-hashes the object named `digest` and checks it still matches.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] when the object is absent,
    /// [`Error::Corrupt`] when its bytes no longer hash to its name, and
    /// [`Error::Hash`] when it cannot be read.
    pub fn verify(&self, digest: &Digest) -> Result<()> {
        let path = self.existing_object(digest)?;
        let actual = Digest::of_file(&path)?;
        if actual == *digest {
            Ok(())
        } else {
            Err(Error::Corrupt {
                expected: *digest,
                actual,
            })
        }
    }

    /// Places the object named `digest` at `destination`, creating missing
    /// parent directories, and reports how it was placed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] when the object is absent,
    /// [`Error::DestinationExists`] when something is already at
    /// `destination`, [`Error::Io`] when a parent directory cannot be created,
    /// and [`Error::Unsupported`] when no permitted method succeeds.
    pub fn materialize(
        &self,
        digest: &Digest,
        destination: impl AsRef<Path>,
        strategy: Strategy,
    ) -> Result<Method> {
        let object = self.existing_object(digest)?;
        let destination = destination.as_ref();
        if fs::symlink_metadata(destination).is_ok() {
            return Err(Error::DestinationExists(destination.to_path_buf()));
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(io_error("create", parent))?;
        }
        materialize::place(&object, destination, strategy)
    }

    fn objects_dir(&self) -> PathBuf {
        self.root.join("objects")
    }

    fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    fn existing_object(&self, digest: &Digest) -> Result<PathBuf> {
        let path = self.object_path(digest);
        if path.is_file() {
            Ok(path)
        } else {
            Err(Error::NotFound(*digest))
        }
    }

    /// A staging path no other process or thread will choose.
    fn staging_path(&self) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        self.tmp_dir()
            .join(format!("{}-{sequence}.tmp", std::process::id()))
    }

    /// Seals a staged file and renames it into place as `digest`.
    fn commit(&self, staging: &Path, digest: Digest) -> Result<()> {
        let object = self.object_path(&digest);
        if object.is_file() {
            // Another writer got there first with the same bytes.
            return remove(staging);
        }

        let mut permissions = fs::metadata(staging)
            .map_err(io_error("inspect", staging))?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(staging, permissions).map_err(io_error("seal", staging))?;

        if let Some(shard) = object.parent() {
            fs::create_dir_all(shard).map_err(io_error("create", shard))?;
        }
        match fs::rename(staging, &object) {
            Ok(()) => Ok(()),
            // Some platforms refuse to rename over an existing read-only
            // file. If the object appeared meanwhile, the race was benign.
            Err(_) if object.is_file() => remove(staging),
            Err(source) => Err(Error::Io {
                action: "rename into place",
                path: object,
                source,
            }),
        }
    }
}

fn create_new(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error("create", path))
}

fn remove(path: &Path) -> Result<()> {
    fs::remove_file(path).map_err(io_error("remove", path))
}

fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io {
        action,
        path,
        source,
    }
}

#[cfg(test)]
mod test;
