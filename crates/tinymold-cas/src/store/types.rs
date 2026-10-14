//! What listing a store reports about the files in it.

use std::path::PathBuf;
use std::time::SystemTime;

use tinymold_hash::Digest;

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
