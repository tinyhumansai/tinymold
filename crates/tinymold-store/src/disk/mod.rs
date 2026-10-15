//! Filesystem helpers the store and its collector share.

use std::fs;
use std::io;
use std::path::Path;

use crate::{Error, Result};

/// The bytes of the file at `path`, or `None` when there is no such file.
pub(crate) fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io {
            action: "read",
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Wraps an I/O failure of `action` on `path`.
pub(crate) fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io {
        action,
        path,
        source,
    }
}
