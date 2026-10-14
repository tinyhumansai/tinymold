//! How tinymold's names map onto REAPI's.
//!
//! - **Blobs** are named by SHA-256, the one digest every REAPI cache
//!   accepts (bazel-remote accepts nothing else).
//! - **`ActionCache` keys** are tinymold's own BLAKE3 digests, sent as opaque
//!   32-byte hashes. Pre-key records are stored under a salted key, so they
//!   can never collide with an action's full key.
//! - **`ByteStream` resources** follow the REAPI resource-name grammar.
//!
//! These names are shared with every other machine using the same server, so
//! the tests pin them.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest as _, Sha256};
use tinymold_hash::{Digest, Hasher};

use crate::proto;

/// The output-file path of the tinymold action record in an `ActionResult`.
pub(crate) const ACTION_FILE: &str = ".tinymold/action";
/// The output-file path of the pre-key record in an `ActionResult`.
pub(crate) const PREKEY_FILE: &str = ".tinymold/prekey";

/// The SHA-256 digest of `bytes`, as REAPI names a blob.
pub(crate) fn blob(bytes: &[u8]) -> proto::Digest {
    let hash = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in hash {
        let _ = write!(hex, "{byte:02x}");
    }
    proto::Digest {
        hash: hex,
        size_bytes: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
    }
}

/// The `ActionCache` key of the action under the full key `key`.
pub(crate) fn action_key(key: &Digest) -> proto::Digest {
    opaque(key)
}

/// The `ActionCache` key of the input sets under the pre-key `pre`.
pub(crate) fn prekey_key(pre: &Digest) -> proto::Digest {
    let mut hasher = Hasher::new();
    hasher.field(b"tinymold/remote/prekey/v1").digest(pre);
    opaque(&hasher.finish())
}

fn opaque(digest: &Digest) -> proto::Digest {
    proto::Digest {
        hash: digest.to_hex(),
        size_bytes: 0,
    }
}

/// The `ByteStream` resource to read the blob `digest` from.
pub(crate) fn read_resource(instance: &str, digest: &proto::Digest) -> String {
    format!(
        "{}blobs/{}/{}",
        prefix(instance),
        digest.hash,
        digest.size_bytes
    )
}

/// A fresh `ByteStream` resource to upload the blob `digest` to.
pub(crate) fn write_resource(instance: &str, digest: &proto::Digest) -> String {
    format!(
        "{}uploads/{}/blobs/{}/{}",
        prefix(instance),
        upload_id(),
        digest.hash,
        digest.size_bytes
    )
}

fn prefix(instance: &str) -> String {
    if instance.is_empty() {
        String::new()
    } else {
        format!("{instance}/")
    }
}

/// A UUID-shaped name for one upload, unique per process and call. Servers
/// only use it to tell concurrent uploads apart.
fn upload_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let mut hasher = Hasher::new();
    hasher
        .field(&std::process::id().to_le_bytes())
        .field(&nanos.to_le_bytes())
        .field(&COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    let hex = hasher.finish().to_hex();
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod test;
