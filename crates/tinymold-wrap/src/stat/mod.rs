//! Knowing a file's digest without reading it, when that is safe.
//!
//! A wrapper call hashes every `--extern` artifact to compute its pre-key, and
//! the same rlibs are read by dozens of dependents per build. The stat cache
//! (`stat/<digest of the path>` in the store) remembers each file's digest
//! next to its identity: device, inode, size and mtime. A file whose identity
//! is unchanged, and whose mtime was written before the entry was, reuses the
//! digest. That last condition is git's racily-clean rule, applied in the
//! filesystem's clock and tick by [`crate::clock`].
//!
//! Where the platform has no inode numbers the cache is never trusted, since
//! size and mtime alone can coincide between two different files.

use std::fs;
use std::path::{Path, PathBuf};

use tinymold_hash::Digest;

use crate::clock::{nanos, tick_of, written_before};
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

/// Whether a digest recorded for `recorded` at `recorded_ns`, a reading of
/// the filesystem clock taken before the file was hashed, still describes a
/// file whose identity is now `current`.
pub(crate) fn still_valid(recorded: Identity, recorded_ns: u64, current: Identity) -> bool {
    current.ino != 0 && recorded == current && written_before(current.mtime_ns, recorded_ns)
}

/// How a file's digest is computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Every byte, as stored objects are named.
    Bytes,
    /// Static archives with their members' build stamps zeroed
    /// ([`tinymold_key::content_digest`]); other files as `Bytes`.
    Archive,
}

/// The stat-cache entry for `path` digested as `kind`.
fn entry_path(store: &Path, path: &Path, kind: Kind) -> PathBuf {
    let tag = match kind {
        Kind::Bytes => "",
        Kind::Archive => "archive:",
    };
    let name = Digest::of_bytes(format!("{tag}{}", path.to_string_lossy()).as_bytes()).to_hex();
    store.join("stat").join(&name[..2]).join(&name[2..])
}

/// The digest of the file at `path`, from the stat cache when its identity is
/// unchanged, otherwise by hashing it (and remembering the result).
///
/// `now_ns` is a reading of the filesystem clock
/// ([`crate::clock::filesystem_now`]) taken before this call.
pub(crate) fn digest(store: &Path, path: &Path, now_ns: u64) -> Result<Digest> {
    digest_as(store, path, Kind::Bytes, now_ns)
}

/// [`digest`], computed as `kind`.
pub(crate) fn digest_as(store: &Path, path: &Path, kind: Kind, now_ns: u64) -> Result<Digest> {
    let current = identity(path)?;
    let entry = entry_path(store, path, kind);
    if let Some((recorded, recorded_ns, digest)) = read(&entry)?.as_deref().and_then(parse_entry)
        && still_valid(recorded, recorded_ns, current)
    {
        return Ok(digest);
    }
    let digest = match kind {
        Kind::Bytes => Digest::of_file(path)?,
        Kind::Archive => tinymold_key::content_digest(&fs::read(path).map_err(io("read", path))?),
    };
    write_entry(store, path, kind, current, digest, now_ns)?;
    Ok(digest)
}

/// Records that the file at `path`, with identity `identity`, has `digest`,
/// trusted from the end of its own timestamp tick.
///
/// Only for a file whose writer has finished, and which nothing rewrites
/// without going through the wrapper again: an output rustc wrote before it
/// exited, or one the wrapper placed itself. Whoever writes it next records
/// its new digest.
pub(crate) fn remember(
    store: &Path,
    path: &Path,
    identity: Identity,
    digest: Digest,
) -> Result<()> {
    let settled_ns = identity.mtime_ns.saturating_add(tick_of(identity.mtime_ns));
    write_entry(store, path, Kind::Bytes, identity, digest, settled_ns)
}

fn write_entry(
    store: &Path,
    path: &Path,
    kind: Kind,
    identity: Identity,
    digest: Digest,
    recorded_ns: u64,
) -> Result<()> {
    let line = format!(
        "{} {} {} {} {} {digest}\n",
        identity.dev, identity.ino, identity.size, identity.mtime_ns, recorded_ns
    );
    write_atomically(&entry_path(store, path, kind), &line)
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
    version(store, program, "-vV")
}

/// The output of `program <flag>`, cached like [`rustc_version`].
pub(crate) fn version(store: &Path, program: &Path, flag: &str) -> Result<String> {
    let resolved = resolve(program).unwrap_or_else(|| program.to_path_buf());
    let identity = identity(&resolved)?;
    let mut key = format!("{} {}", resolved.display(), identity_text(identity));
    if flag != "-vV" {
        // rustc's entries predate the flag; every other flag is named.
        key.push(' ');
        key.push_str(flag);
    }
    let entry = store
        .join("rustc")
        .join(Digest::of_bytes(key.as_bytes()).to_hex());
    if let Some(version) = read(&entry)? {
        return Ok(version);
    }
    let output = std::process::Command::new(program)
        .arg(flag)
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
