//! Signed remote entries: writers sign what they push, readers use only what
//! a trusted key signed.
//!
//! A cache server decides who may write, and a server set up wrong lets
//! anyone write. With [`TrustedKeys`] configured, the client stops depending
//! on that: an entry that a trusted [`SigningKey`] did not sign is refused,
//! so the worst an open server can do is make a build miss.
//!
//! The signature is a blob of its own, listed in the entry's `ActionResult`
//! as [`SIGNATURE_FILE`]. It signs [`message`]: the entry's kind, its
//! `ActionCache` key and the SHA-256 of its record. The record names every
//! object by BLAKE3, and lookup refuses an object whose content does not
//! match that name, so the signature covers the whole entry. The encodings
//! are shared with every machine using the same server; the tests pin them.

use std::fmt::{self, Write as _};

use ed25519_compact::{KeyPair, PublicKey, Seed, Signature};

use crate::proto;
use crate::{Error, Result};

/// The output-file path of an entry's signature in an `ActionResult`.
pub(crate) const SIGNATURE_FILE: &str = ".tinymold/signature";

/// What an entry holds, which its signature names so that a signature over
/// one kind of entry never validates the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// An action: a unit's outputs under its full key.
    Action,
    /// A pre-key record: the input sets under a pre-key.
    Prekey,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::Prekey => "prekey",
        }
    }
}

/// The bytes signed for the entry of `kind` under the `ActionCache` key
/// `key`, whose record is the blob `record`.
pub(crate) fn message(kind: Kind, key: &proto::Digest, record: &proto::Digest) -> Vec<u8> {
    format!(
        "tinymold/remote/entry/v1\n{}\n{}\n{}/{}\n",
        kind.name(),
        key.hash,
        record.hash,
        record.size_bytes
    )
    .into_bytes()
}

/// A writer's ed25519 secret key, from `TINYMOLD_REMOTE_SIGNING_KEY`.
#[derive(Clone, PartialEq, Eq)]
pub struct SigningKey {
    seed: [u8; 32],
}

impl fmt::Debug for SigningKey {
    // The secret never appears in logs or panics.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKey")
            .field("public", &self.public_hex())
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    /// A fresh key from the operating system's random source. This is
    /// `tinymold remote keygen`.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            seed: *Seed::generate(),
        }
    }

    /// The key written as 64 hex digits.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSigningKey`] when `text` is not 64 hex digits.
    pub fn from_hex(text: &str) -> Result<Self> {
        let seed = parse_hex(text.trim()).ok_or(Error::InvalidSigningKey)?;
        Ok(Self { seed })
    }

    /// The secret key as 64 hex digits, as `TINYMOLD_REMOTE_SIGNING_KEY`
    /// takes it.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.seed)
    }

    /// The matching public key as 64 hex digits, as
    /// `TINYMOLD_REMOTE_TRUSTED_KEYS` takes it.
    #[must_use]
    pub fn public_hex(&self) -> String {
        hex(self.pair().pk.as_ref())
    }

    fn pair(&self) -> KeyPair {
        KeyPair::from_seed(Seed::new(self.seed))
    }

    /// The signature blob for the entry of `kind` under `key` whose record
    /// is `record`.
    pub(crate) fn sign(&self, kind: Kind, key: &proto::Digest, record: &proto::Digest) -> Vec<u8> {
        let pair = self.pair();
        // Deterministic (no noise): the same entry always signs the same way,
        // so pushing it again uploads nothing new.
        let signature = pair.sk.sign(message(kind, key, record), None);
        format!(
            "tinymold-signature 1\nkey {}\nsig {}\n",
            hex(pair.pk.as_ref()),
            hex(signature.as_ref())
        )
        .into_bytes()
    }
}

/// The public keys whose signatures a reader accepts, from
/// `TINYMOLD_REMOTE_TRUSTED_KEYS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKeys {
    keys: Vec<[u8; 32]>,
}

impl TrustedKeys {
    /// Comma-separated public keys of 64 hex digits each.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidTrustedKey`] naming the first entry that is not
    /// 64 hex digits or not a valid ed25519 public key, or when there is
    /// none.
    pub fn from_list(text: &str) -> Result<Self> {
        let mut keys = Vec::new();
        for item in text
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            let key = parse_hex(item)
                .filter(|key| PublicKey::from_slice(key).is_ok())
                .ok_or_else(|| Error::InvalidTrustedKey(item.to_owned()))?;
            keys.push(key);
        }
        if keys.is_empty() {
            return Err(Error::InvalidTrustedKey(text.to_owned()));
        }
        Ok(Self { keys })
    }

    /// Whether `blob` is a signature by a trusted key over the entry of
    /// `kind` under `key` whose record is `record`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Untrusted`] saying why when it is not.
    pub(crate) fn verify(
        &self,
        kind: Kind,
        key: &proto::Digest,
        record: &proto::Digest,
        blob: &[u8],
    ) -> Result<()> {
        let untrusted = |reason| Error::Untrusted {
            key: key.hash.clone(),
            reason,
        };
        let text = std::str::from_utf8(blob).map_err(|_| untrusted("its signature is not text"))?;
        let mut lines = text.lines();
        let (Some("tinymold-signature 1"), Some(public), Some(signature), None) = (
            lines.next(),
            lines.next().and_then(|line| line.strip_prefix("key ")),
            lines.next().and_then(|line| line.strip_prefix("sig ")),
            lines.next(),
        ) else {
            return Err(untrusted("its signature is malformed"));
        };
        let public: [u8; 32] =
            parse_hex(public).ok_or_else(|| untrusted("its signature is malformed"))?;
        let signature: [u8; 64] =
            parse_hex(signature).ok_or_else(|| untrusted("its signature is malformed"))?;
        if !self.keys.contains(&public) {
            return Err(untrusted("it is signed by a key that is not trusted"));
        }
        let public =
            PublicKey::from_slice(&public).map_err(|_| untrusted("its signature is malformed"))?;
        public
            .verify(message(kind, key, record), &Signature::new(signature))
            .map_err(|_| untrusted("its signature does not match it"))
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Exactly `N` bytes written as `2 * N` hex digits, either case.
fn parse_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.is_ascii() {
        return None;
    }
    let mut bytes = [0; N];
    for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(bytes)
}

#[cfg(test)]
mod test;
