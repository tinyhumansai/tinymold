//! Digests of static archives that ignore when they were built.
//!
//! Build scripts compile C libraries into `.a` archives that rustc links into
//! dependents. `ar` stamps every member header with the build's time, owner
//! and group, so two builds of the same sources produce archives that differ
//! only in those fields: measured on bat's `libgit2.a`, all 196 members were
//! byte-identical once the stamps were zeroed. Linking never reads them, so
//! [`content_digest`] hashes an archive with them zeroed, and every dependent
//! of a rebuilt `-sys` crate keeps its key.
//!
//! The format is the common `ar` one: an `!<arch>\n` magic, then for each
//! member a 60-byte header (name 16, mtime 12, uid 6, gid 6, mode 8, size 10,
//! terminator 2) followed by the data, padded to an even length. Anything that
//! is not such an archive, or does not parse as one, is hashed as it is.

use tinymold_hash::Digest;

const MAGIC: &[u8] = b"!<arch>\n";
const HEADER: usize = 60;

/// The digest of `bytes`, with the time, owner and group of every member
/// zeroed when `bytes` is an `ar` archive.
#[must_use]
pub fn content_digest(bytes: &[u8]) -> Digest {
    normalized(bytes).map_or_else(|| Digest::of_bytes(bytes), |clean| Digest::of_bytes(&clean))
}

fn normalized(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.starts_with(MAGIC) {
        return None;
    }
    let mut clean = bytes.to_vec();
    let mut at = MAGIC.len();
    while at < clean.len() {
        let header = clean.get_mut(at..at + HEADER)?;
        if &header[58..60] != b"`\n" {
            return None;
        }
        let size: usize = std::str::from_utf8(&header[48..58]).ok()?.trim().parse().ok()?;
        for field in [16..28, 28..34, 34..40] {
            let width = field.len();
            header[field].copy_from_slice(format!("{:<width$}", "0").as_bytes());
        }
        at = at.checked_add(HEADER + size + size % 2)?;
    }
    (at == clean.len() || at == clean.len() + 1).then_some(clean)
}

#[cfg(test)]
mod test;
