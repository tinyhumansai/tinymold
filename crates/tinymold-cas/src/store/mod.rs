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
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use tinymold_hash::Digest;

mod types;

pub use types::{ObjectInfo, Staged, StagingFile};

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

    /// Whether an object named `digest` is in the store, refreshing its mtime
    /// as storing the same bytes again would. A writer about to reference an
    /// object it has not re-inserted calls this, so the object counts as
    /// freshly stored for `tinymold gc`'s grace period.
    #[must_use]
    pub fn keep(&self, digest: &Digest) -> bool {
        let object = self.object_path(digest);
        if !object.is_file() {
            return false;
        }
        touch(&object);
        true
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
            touch(&self.object_path(&digest));
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

    /// Streams `reader` into a private staging file and hashes it on the way,
    /// without storing it yet.
    ///
    /// The returned [`Staged`] body knows its digest and size, so a caller
    /// can check them against what a client claimed before deciding.
    /// [`Staged::commit`] stores it; dropping it discards it. The bytes are
    /// read once and never held in memory as a whole, which is what lets a
    /// server accept an upload of any size.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Stream`] when `reader` fails, and [`Error::Io`] when
    /// the staging file cannot be written. Neither leaves a staging file.
    pub fn stage(&self, reader: impl Read) -> Result<Staged<'_>> {
        let path = self.staging_path();
        let file = create_new(&path)?;
        let mut tee = Tee {
            reader,
            file,
            size: 0,
            failure: None,
        };
        let hashed = Digest::of_reader(&mut tee);
        let Tee { size, failure, .. } = tee;
        // Every read error passes through the tee, which keeps the cause;
        // the hasher's own error only says that a read failed.
        let outcome = match (failure, hashed) {
            (None, Ok(digest)) => Ok(digest),
            (failure, hashed) => Err(failure.map_or_else(
                || {
                    hashed.err().map_or(
                        Error::Stream(io::Error::other("stream failed")),
                        Error::from,
                    )
                },
                |failure| failure.into_error(&path),
            )),
        };
        match outcome {
            Ok(digest) => Ok(Staged {
                store: self,
                path,
                digest,
                size,
            }),
            Err(error) => {
                // Best effort: a stale staging file is garbage, not a fault.
                let _ = fs::remove_file(&path);
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

    /// Lists every object in the store, in no particular order.
    ///
    /// Files under `objects/` whose path is not a digest are skipped: they
    /// are not objects, whoever put them there.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the objects directory or a shard cannot be
    /// read.
    pub fn objects(&self) -> Result<Vec<ObjectInfo>> {
        let dir = self.objects_dir();
        let mut objects = Vec::new();
        for shard in read_dir(&dir)? {
            let Some(prefix) = shard.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let shard_path = shard.path();
            if prefix.len() != 2 || !shard_path.is_dir() {
                continue;
            }
            for entry in read_dir(&shard_path)? {
                let Some(rest) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Ok(digest) = Digest::from_str(&format!("{prefix}{rest}")) else {
                    continue;
                };
                // Removed by someone else since the directory was read.
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if meta.is_file() {
                    objects.push(ObjectInfo {
                        digest,
                        size: meta.len(),
                        modified: meta
                            .modified()
                            .map_err(io_error("inspect", &entry.path()))?,
                    });
                }
            }
        }
        Ok(objects)
    }

    /// Removes the object named `digest`, and reports whether it was there.
    ///
    /// Removing an object an action still names makes that action's next
    /// restore fail; deciding what is safe to remove is the caller's job.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the object exists but cannot be removed.
    pub fn remove(&self, digest: &Digest) -> Result<bool> {
        remove_if_present(&self.object_path(digest))
    }

    /// Lists the files in the staging directory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the staging directory cannot be read.
    pub fn staging_files(&self) -> Result<Vec<StagingFile>> {
        let mut files = Vec::new();
        for entry in read_dir(&self.tmp_dir())? {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_file() {
                files.push(StagingFile {
                    path: entry.path(),
                    size: meta.len(),
                    modified: meta
                        .modified()
                        .map_err(io_error("inspect", &entry.path()))?,
                });
            }
        }
        Ok(files)
    }

    /// Removes a staging file [`Store::staging_files`] listed, and reports
    /// whether it was still there.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file exists but cannot be removed.
    pub fn remove_staging(&self, file: &StagingFile) -> Result<bool> {
        remove_if_present(&file.path)
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
            // Already stored, perhaps by another writer just now.
            touch(&object);
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
            Err(_) if object.is_file() => {
                touch(&object);
                remove(staging)
            }
            Err(source) => Err(Error::Io {
                action: "rename into place",
                path: object,
                source,
            }),
        }
    }
}

impl Staged<'_> {
    /// The BLAKE3 digest of the staged bytes.
    #[must_use]
    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// The number of staged bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Seals the staged bytes and stores them under their digest.
    ///
    /// Committing bytes the store already holds keeps the existing object
    /// and discards the staged copy.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the object cannot be sealed or renamed
    /// into place.
    pub fn commit(mut self) -> Result<Digest> {
        let path = std::mem::take(&mut self.path);
        self.store.commit(&path, self.digest)?;
        Ok(self.digest)
    }
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        // An empty path means `commit` already took the file. Otherwise the
        // body was abandoned, and its staging file is removed best effort.
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Copies everything read through it into a staging file, so one pass over
/// a stream both hashes it and stores it.
struct Tee<R> {
    reader: R,
    file: File,
    size: u64,
    /// The first failure, kept apart from the hasher's error so a broken
    /// stream and a broken disk are reported as what they are.
    failure: Option<TeeFailure>,
}

enum TeeFailure {
    Read(io::Error),
    Write(io::Error),
}

impl TeeFailure {
    fn into_error(self, staging: &Path) -> Error {
        match self {
            Self::Read(source) => Error::Stream(source),
            Self::Write(source) => io_error("write", staging)(source),
        }
    }
}

impl<R: Read> Read for Tee<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = loop {
            match self.reader.read(buffer) {
                Ok(read) => break read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    let reported = io::Error::new(error.kind(), "stream failed");
                    self.failure = Some(TeeFailure::Read(error));
                    return Err(reported);
                }
            }
        };
        if let Err(error) = self.file.write_all(&buffer[..read]) {
            let reported = io::Error::new(error.kind(), "staging write failed");
            self.failure = Some(TeeFailure::Write(error));
            return Err(reported);
        }
        // `usize` is at most 64 bits on every supported target.
        self.size += read as u64;
        Ok(read)
    }
}

fn create_new(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error("create", path))
}

/// The entries of `dir`, skipping ones that cannot be read; empty when `dir`
/// does not exist.
fn read_dir(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(entries.filter_map(std::result::Result::ok).collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(Error::Io {
            action: "read",
            path: dir.to_path_buf(),
            source,
        }),
    }
}

fn remove_if_present(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        // Windows refuses to delete a read-only file, and objects are sealed.
        Err(_) if unseal(path) => fs::remove_file(path).map(|()| true).map_err(|source| {
            // Still there, so seal it again: an object must never be left
            // writable.
            set_readonly(path, true);
            io_error("remove", path)(source)
        }),
        Err(source) => Err(Error::Io {
            action: "remove",
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Sets an existing object's mtime to now, best effort. An object's mtime
/// is when it was last stored, which is what keeps one an ingest is about to
/// reference inside `tinymold gc`'s grace period.
fn touch(path: &Path) {
    let mut options = fs::OpenOptions::new();
    // Objects are read-only. Unix sets times through any descriptor of a
    // file the caller owns; Windows needs a handle with write-attributes
    // access.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        options.access_mode(FILE_WRITE_ATTRIBUTES);
    }
    #[cfg(not(windows))]
    options.read(true);
    if let Ok(file) = options.open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// Clears the read-only flag on `path`, and reports whether it did.
fn unseal(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    meta.permissions().readonly() && set_readonly(path, false)
}

/// Sets or clears the read-only flag on `path`, and reports whether it did.
fn set_readonly(path: &Path, readonly: bool) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let mut permissions = meta.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(readonly);
    fs::set_permissions(path, permissions).is_ok()
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
