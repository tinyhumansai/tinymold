//! The [`Digest`] type and the ways to compute one.
//!
//! A digest is the BLAKE3-256 hash of some bytes. It is the identity of those
//! bytes everywhere in tinymold: two digests are equal exactly when the bytes
//! they name are (up to BLAKE3's collision resistance).
//!
//! Small inputs are hashed with [`Digest::of_bytes`], streams with
//! [`Digest::of_reader`], files with [`Digest::of_file`], and inputs assembled
//! piece by piece — such as a cache key built from many fields — with
//! [`Hasher`].

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::{Error, Result};

/// The BLAKE3-256 digest of a sequence of bytes.
///
/// The text form is 64 lowercase hexadecimal characters, the same encoding
/// `b3sum` prints, so digests can be checked against standard tools.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; Digest::LEN]);

impl Digest {
    /// The length of a digest in bytes.
    pub const LEN: usize = 32;

    /// Wraps raw digest bytes, for digests decoded from a manifest or the wire.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// Returns the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// Hashes an in-memory byte slice.
    ///
    /// ```
    /// # use tinymold_hash::Digest;
    /// assert_eq!(Digest::of_bytes(b"abc"), Digest::of_bytes(b"abc"));
    /// assert_ne!(Digest::of_bytes(b"abc"), Digest::of_bytes(b"abd"));
    /// ```
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Hashes everything `reader` yields until end of stream.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Read`] with the path `<reader>` when reading fails.
    pub fn of_reader(reader: impl Read) -> Result<Self> {
        hash_reader(reader, Path::new("<reader>"))
    }

    /// Hashes the contents of the file at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Read`] naming `path` when the file cannot be opened or
    /// read.
    pub fn of_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|source| read_error(path, source))?;
        hash_reader(file, path)
    }

    /// Returns the 64-character lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

impl FromStr for Digest {
    type Err = Error;

    /// Parses the 64-character hexadecimal form, in either letter case.
    fn from_str(text: &str) -> Result<Self> {
        let invalid = || Error::InvalidHex {
            text: text.to_owned(),
        };
        if text.len() != Self::LEN * 2 {
            return Err(invalid());
        }

        let mut bytes = [0_u8; Self::LEN];
        for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
            let high = hex_value(pair[0]).ok_or_else(invalid)?;
            let low = hex_value(pair[1]).ok_or_else(invalid)?;
            *byte = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

/// Builds one digest from many pieces.
///
/// Each piece is length-prefixed before it is hashed, so the boundaries
/// between pieces are part of the digest: `["ab", "c"]` and `["a", "bc"]`
/// produce different digests. That is what a cache key needs, since its fields
/// must not be able to bleed into one another.
///
/// ```
/// # use tinymold_hash::Hasher;
/// let mut a = Hasher::new();
/// a.field(b"ab").field(b"c");
/// let mut b = Hasher::new();
/// b.field(b"a").field(b"bc");
/// assert_ne!(a.finish(), b.finish());
/// ```
#[derive(Debug, Clone, Default)]
pub struct Hasher(blake3::Hasher);

impl Hasher {
    /// Starts an empty hasher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one length-prefixed field.
    pub fn field(&mut self, bytes: &[u8]) -> &mut Self {
        // `usize` is at most 64 bits on every supported target, so widening
        // to `u64` is lossless and keeps the encoding platform-independent.
        let len = bytes.len() as u64;
        self.0.update(&len.to_le_bytes());
        self.0.update(bytes);
        self
    }

    /// Appends another digest as one field.
    pub fn digest(&mut self, digest: &Digest) -> &mut Self {
        self.field(digest.as_bytes())
    }

    /// Returns the digest of every field appended so far.
    #[must_use]
    pub fn finish(&self) -> Digest {
        Digest(*self.0.finalize().as_bytes())
    }
}

/// Buffer size for streaming reads: large enough that BLAKE3's SIMD paths see
/// whole multi-chunk batches, small enough to stay in L2.
const READ_BUFFER: usize = 64 * 1024;

fn hash_reader(mut reader: impl Read, path: &Path) -> Result<Digest> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; READ_BUFFER];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                hasher.update(&buffer[..read]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(source) => return Err(read_error(path, source)),
        }
    }
    Ok(Digest(*hasher.finalize().as_bytes()))
}

fn read_error(path: &Path, source: std::io::Error) -> Error {
    Error::Read {
        path: PathBuf::from(path),
        source,
    }
}

const fn hex_value(character: u8) -> Option<u8> {
    match character {
        b'0'..=b'9' => Some(character - b'0'),
        b'a'..=b'f' => Some(character - b'a' + 10),
        b'A'..=b'F' => Some(character - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod test;
