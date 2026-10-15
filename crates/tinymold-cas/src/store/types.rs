//! What listing a store reports about the files in it.

use std::path::PathBuf;
use std::time::SystemTime;

use tinymold_hash::Digest;

use crate::Store;

/// One object in a store, as [`Store::objects`](crate::Store::objects) lists
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    /// The object's name.
    pub digest: Digest,
    /// Its size in bytes.
    pub size: u64,
    /// When it was last stored: committed, or stored again while already
    /// present. Objects are immutable, so this is their mtime.
    pub modified: SystemTime,
}

/// A file left in the store's staging directory, as
/// [`Store::staging_files`](crate::Store::staging_files) lists it.
///
/// An insert in progress owns its staging file; one that is old is garbage
/// left by a process that died mid-insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagingFile {
    /// Where the file is.
    pub path: PathBuf,
    /// Its size in bytes.
    pub size: u64,
    /// When it was last written.
    pub modified: SystemTime,
}

/// A body streamed into the store's staging area by
/// [`Store::stage`](crate::Store::stage), hashed but not yet stored.
///
/// [`Staged::commit`] stores it under [`Staged::digest`]. Dropping it
/// without committing removes the staging file, so an upload that is
/// rejected or abandoned leaves nothing behind.
#[derive(Debug)]
#[must_use = "a staged body is discarded unless it is committed"]
pub struct Staged<'a> {
    pub(super) store: &'a Store,
    pub(super) path: PathBuf,
    pub(super) digest: Digest,
    pub(super) size: u64,
}
