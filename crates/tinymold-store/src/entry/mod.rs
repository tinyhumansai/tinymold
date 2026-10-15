//! Entry records: what one store key holds.
//!
//! The encoding is binary, versioned by its first eight bytes, and pinned by
//! a unit test, because every entry ever written must keep decoding:
//!
//! ```text
//! "tmentry1"                      magic and version
//! u64 written                     unix seconds
//! u32 n, n × [u8; 32]             blob digests, in order
//! u64 len, len bytes              body
//! u32 m, m × (u32 len, key, u32 len, value)
//!                                 metadata, keys strictly ascending
//! ```
//!
//! All integers are little-endian. Nothing may follow the last field.

use std::collections::BTreeMap;

use tinymold_hash::Digest;

use crate::{Error, Result};

/// The largest metadata an entry may carry: the sum of every key and value
/// length, in bytes.
pub const MAX_META: usize = 4096;

const MAGIC: &[u8; 8] = b"tmentry1";

/// What a store key holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// The objects the entry needs, in order. One for protocols that store a
    /// single artifact; one per output for REAPI.
    pub blobs: Vec<Digest>,
    /// Bytes the front-end returns as they were stored: a REAPI
    /// `ActionResult`. Empty for single-artifact protocols.
    pub body: Vec<u8>,
    /// Small values a front-end must hand back on a read, such as Turbo's
    /// artifact headers.
    pub meta: BTreeMap<String, String>,
    /// When the entry was written, in unix seconds. Informational only:
    /// no lookup reads it.
    pub written: u64,
}

impl Entry {
    /// An entry holding `blobs`, with no body or metadata.
    #[must_use]
    pub fn new(blobs: Vec<Digest>) -> Self {
        Self {
            blobs,
            ..Self::default()
        }
    }

    /// Encodes the entry in its stored form.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MetaTooLarge`] when the metadata exceeds [`MAX_META`]
    /// bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let meta_size: usize = self.meta.iter().map(|(k, v)| k.len() + v.len()).sum();
        if meta_size > MAX_META {
            return Err(Error::MetaTooLarge { size: meta_size });
        }

        let mut out = Vec::with_capacity(
            MAGIC.len() + 24 + self.blobs.len() * Digest::LEN + self.body.len() + meta_size,
        );
        out.extend(MAGIC);
        out.extend(self.written.to_le_bytes());
        push_u32(&mut out, self.blobs.len());
        for blob in &self.blobs {
            out.extend(blob.as_bytes());
        }
        // `usize` is at most 64 bits on every supported target.
        out.extend((self.body.len() as u64).to_le_bytes());
        out.extend(&self.body);
        push_u32(&mut out, self.meta.len());
        for (key, value) in &self.meta {
            push_u32(&mut out, key.len());
            out.extend(key.as_bytes());
            push_u32(&mut out, value.len());
            out.extend(value.as_bytes());
        }
        Ok(out)
    }

    /// Decodes an entry from its stored form.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedEntry`] when `bytes` is not a complete,
    /// canonical entry record.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = Reader(bytes);
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(malformed("unknown format"));
        }
        let written = reader.u64()?;

        let count = reader.u32()?;
        let mut blobs = Vec::with_capacity(count.min(bytes.len() / Digest::LEN));
        for _ in 0..count {
            let raw: [u8; Digest::LEN] = reader
                .take(Digest::LEN)?
                .try_into()
                .map_err(|_| malformed("truncated"))?;
            blobs.push(Digest::from_bytes(raw));
        }

        let body_len = usize::try_from(reader.u64()?).map_err(|_| malformed("truncated"))?;
        let body = reader.take(body_len)?.to_vec();

        let mut meta = BTreeMap::new();
        let mut previous: Option<String> = None;
        for _ in 0..reader.u32()? {
            let key = reader.text()?;
            let value = reader.text()?;
            if previous.as_ref().is_some_and(|last| *last >= key) {
                return Err(malformed("metadata out of order"));
            }
            previous = Some(key.clone());
            meta.insert(key, value);
        }

        if !reader.0.is_empty() {
            return Err(malformed("trailing bytes"));
        }
        Ok(Self {
            blobs,
            body,
            meta,
            written,
        })
    }
}

fn push_u32(out: &mut Vec<u8>, value: usize) {
    // Counts and lengths are bounded by `MAX_META` and by what fits in
    // memory; an entry with four billion blobs is not a real entry.
    let value = u32::try_from(value).unwrap_or(u32::MAX);
    out.extend(value.to_le_bytes());
}

fn malformed(reason: &'static str) -> Error {
    Error::MalformedEntry { reason }
}

/// A cursor over the bytes being decoded.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.0.len() < len {
            return Err(malformed("truncated"));
        }
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(head)
    }

    fn u32(&mut self) -> Result<usize> {
        let raw: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| malformed("truncated"))?;
        usize::try_from(u32::from_le_bytes(raw)).map_err(|_| malformed("truncated"))
    }

    fn u64(&mut self) -> Result<u64> {
        let raw: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| malformed("truncated"))?;
        Ok(u64::from_le_bytes(raw))
    }

    fn text(&mut self) -> Result<String> {
        let len = self.u32()?;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| malformed("metadata is not utf-8"))
    }
}

#[cfg(test)]
mod test;
