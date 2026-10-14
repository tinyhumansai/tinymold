//! Knowing a file's digest without reading it, when that is safe.
//!
//! A wrapper call hashes every `--extern` artifact to compute its pre-key, and
//! the same rlibs are read by dozens of dependents per build. The stat cache
//! (`stat/<digest of the path>` in the store) remembers each file's digest
//! next to its identity: device, inode, size and mtime. A file whose identity
//! is unchanged, and whose mtime is older than the entry, reuses the digest.
//! That last condition is git's racily-clean rule: a file modified within the
//! same timestamp tick it was hashed in could have changed unseen.
//!
//! Where the platform has no inode numbers the cache is never trusted, since
//! size and mtime alone can coincide between two different files.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tinymold_hash::Digest;

use crate::error::io;
use crate::record::{Identity, read, write_atomically};
use crate::{Error, Result};

/// The identity of the file at `path`, following symlinks.
pub(crate) fn identity(path: &Path) -> Result<Identity> {
    let meta = fs::metadata(path).map_err(io("read", path))?;
    Ok(identity_of(&meta))
}

pub(crate) fn identity_of(meta: &fs::Metadata) -> Identity {
    let (dev, ino) = device_and_inode(meta);
    Identity {
        dev,
        ino,
        size: meta.len(),
        mtime_ns: meta.modified().map_or(0, nanos),
    }
}

#[cfg(unix)]
fn device_and_inode(meta: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn device_and_inode(_meta: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// Nanoseconds since the Unix epoch; 0 before it, saturating far after it.
pub(crate) fn nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_nanos()).unwrap_or(u64::MAX))
}

/// Whether a digest recorded for `recorded` at `recorded_ns` still describes a
/// file whose identity is now `current`.
pub(crate) fn still_valid(recorded: Identity, recorded_ns: u64, current: Identity) -> bool {
    current.ino != 0 && recorded == current && current.mtime_ns < recorded_ns
}

/// The stat-cache entry for `path`.
fn entry_path(store: &Path, path: &Path) -> PathBuf {
    let name = Digest::of_bytes(path.to_string_lossy().as_bytes()).to_hex();
    store.join("stat").join(&name[..2]).join(&name[2..])
}

/// The digest of the file at `path`, from the stat cache when its identity is
/// unchanged, otherwise by hashing it (and remembering the result).
pub(crate) fn digest(store: &Path, path: &Path) -> Result<Digest> {
    let current = identity(path)?;
    let entry = entry_path(store, path);
    if let Some((recorded, recorded_ns, digest)) = read(&entry)?.as_deref().and_then(parse_entry)
        && still_valid(recorded, recorded_ns, current)
    {
        return Ok(digest);
    }
    let digest = Digest::of_file(path)?;
    remember(store, path, current, digest)?;
    Ok(digest)
}

/// Records that the file at `path`, with identity `identity`, has `digest`.
/// Used after hashing, and after restoring an output whose digest is known.
pub(crate) fn remember(store: &Path, path: &Path, identity: Identity, digest: Digest) -> Result<()> {
    let line = format!(
        "{} {} {} {} {} {digest}\n",
        identity.dev,
        identity.ino,
        identity.size,
        identity.mtime_ns,
        nanos(SystemTime::now())
    );
    write_atomically(&entry_path(store, path), &line)
}

fn parse_entry(text: &str) -> Option<(Identity, u64, Digest)> {
    let mut parts = text.trim_end().split(' ');
    let mut number = || parts.next()?.parse::<u64>().ok();
    let identity = Identity {
        dev: number()?,
        ino: number()?,
        size: number()?,
        mtime_ns: number()?,
    };
    let recorded_ns = number()?;
    let digest = parts.next()?.parse().ok()?;
    Some((identity, recorded_ns, digest))
}

/// `program -vV`, cached in the store by the program's identity so that it
/// runs once per toolchain rather than once per crate.
pub(crate) fn rustc_version(store: &Path, program: &Path) -> Result<String> {
    let resolved = resolve(program).unwrap_or_else(|| program.to_path_buf());
    let identity = identity(&resolved)?;
    let key = Digest::of_bytes(
        format!("{} {}", resolved.display(), identity_text(identity)).as_bytes(),
    )
    .to_hex();
    let entry = store.join("rustc").join(key);
    if let Some(version) = read(&entry)? {
        return Ok(version);
    }
    let output = std::process::Command::new(program)
        .arg("-vV")
        .output()
        .map_err(|source| Error::Spawn {
            program: program.to_path_buf(),
            source,
        })?;
    let version = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        write_atomically(&entry, &version)?;
    }
    Ok(version)
}

/// The identity of the linker a linking unit uses: its resolved path, size
/// and mtime, or a marker naming it when it cannot be found.
pub(crate) fn linker_identity(linker: &Path, cwd: &Path) -> String {
    let candidate = if linker.components().count() > 1 {
        Some(cwd.join(linker))
    } else {
        resolve(linker)
    };
    candidate
        .and_then(|path| Some((identity(&path).ok()?, path)))
        .map_or_else(
            || format!("missing {}", linker.display()),
            |(identity, path)| format!("{} {}", path.display(), identity_text(identity)),
        )
}

fn identity_text(identity: Identity) -> String {
    format!("{} {}", identity.size, identity.mtime_ns)
}

/// `program` resolved against `PATH` when it is a bare name.
fn resolve(program: &Path) -> Option<PathBuf> {
    if program.components().count() > 1 {
        return Some(program.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod test;
