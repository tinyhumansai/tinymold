//! SHA-256 digests, for protocols that name blobs by them.
//!
//! REAPI and Bazel's HTTP cache identify blobs by SHA-256. tinymold names
//! every object by BLAKE3 ([ADR 0002]), so SHA-256 appears only on the wire
//! and in the store's index from SHA-256 to the BLAKE3 object name.
//!
//! [ADR 0002]: https://github.com/tinyhumansai/tinymold/blob/main/docs/adr/0002-blake3-for-content-digests.md

use std::fmt;
use std::str::FromStr;

use sha2::Digest as _;

use crate::{Error, Result};

/// A SHA-256 digest. Its text form is 64 lowercase hexadecimal digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256([u8; Sha256::LEN]);

impl Sha256 {
    /// The length of a digest in bytes.
    pub const LEN: usize = 32;

    /// Wraps raw digest bytes, for digests decoded from the wire.
    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// Hashes an in-memory byte slice.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(sha2::Sha256::digest(bytes).into())
    }

    /// The 64-character lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut hex = String::with_capacity(Self::LEN * 2);
        for byte in self.0 {
            hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
            hex.push(char::from(DIGITS[usize::from(byte & 0xf)]));
        }
        hex
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256({})", self.to_hex())
    }
}

impl FromStr for Sha256 {
    type Err = Error;

    /// Parses the 64-character hexadecimal form, in either letter case.
    fn from_str(text: &str) -> Result<Self> {
        let invalid = || Error::InvalidSha256(text.to_owned());
        let bytes = text.as_bytes();
        if bytes.len() != Self::LEN * 2 {
            return Err(invalid());
        }
        let mut out = [0_u8; Self::LEN];
        for (slot, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
            let high = nibble(pair[0]).ok_or_else(invalid)?;
            let low = nibble(pair[1]).ok_or_else(invalid)?;
            *slot = high << 4 | low;
        }
        Ok(Self(out))
    }
}

fn nibble(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}

/// Hashes a stream as it is read, for naming a body by SHA-256 in the same
/// pass that stores it.
#[derive(Debug)]
pub(crate) struct Sha256Reader<R> {
    inner: R,
    hasher: sha2::Sha256,
}

impl<R> Sha256Reader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: sha2::Sha256::new(),
        }
    }

    pub(crate) fn finish(self) -> Sha256 {
        Sha256(self.hasher.finalize().into())
    }
}

impl<R: std::io::Read> std::io::Read for Sha256Reader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        Ok(read)
    }
}

#[cfg(test)]
mod test;
