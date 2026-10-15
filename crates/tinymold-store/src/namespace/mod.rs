//! Namespaces and the store keys derived from them.
//!
//! A client key means something only to the protocol and tenant that wrote
//! it: Turbo's 16 hex digits, Gradle's task hash, a REAPI action digest. The
//! store therefore never indexes a client key directly. It derives a
//! fixed-size [`Key`] from the front-end, the tenant and the key bytes, so
//! two protocols, or two teams, that send the same bytes never share an
//! entry.

use std::fmt;

use tinymold_hash::{Digest, Hasher};

use crate::{Error, Result};

/// The longest tenant accepted, in bytes.
pub const MAX_TENANT: usize = 255;

/// The longest client key accepted, in bytes.
pub const MAX_CLIENT_KEY: usize = 1024;

/// Field that versions the store key derivation. Changing it, or the order
/// of the fields after it, orphans every entry stored before.
const KEY_DOMAIN: &[u8] = b"tinymold/store/ac/v1";

/// The protocol a key arrived through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FrontEnd {
    /// The Remote Execution API v2 over gRPC.
    Reapi,
    /// Bazel's HTTP cache protocol (`/ac/`, `/cas/`), also used by ccache.
    BazelHttp,
    /// Turborepo's remote cache API.
    Turbo,
    /// Nx's self-hosted remote cache API.
    Nx,
    /// Gradle's HTTP build cache.
    Gradle,
    /// tinymold's own clients: the Rust and Go adapters.
    Native,
}

impl FrontEnd {
    /// Every front-end, in a fixed order.
    pub const ALL: [Self; 6] = [
        Self::Reapi,
        Self::BazelHttp,
        Self::Turbo,
        Self::Nx,
        Self::Gradle,
        Self::Native,
    ];

    /// The front-end's stable name, as it appears in store keys and
    /// configuration.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reapi => "reapi",
            Self::BazelHttp => "bazel-http",
            Self::Turbo => "turbo",
            Self::Nx => "nx",
            Self::Gradle => "gradle",
            Self::Native => "native",
        }
    }
}

impl fmt::Display for FrontEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A front-end and a tenant: the scope a client key is meaningful in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Namespace {
    front_end: FrontEnd,
    tenant: String,
}

impl Namespace {
    /// Builds the namespace for `tenant` within `front_end`.
    ///
    /// The tenant is the protocol's own scoping: a REAPI instance name, a
    /// Turbo team, or a URL prefix. It may be empty.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TenantTooLong`] when `tenant` is longer than
    /// [`MAX_TENANT`] bytes.
    pub fn new(front_end: FrontEnd, tenant: impl Into<String>) -> Result<Self> {
        let tenant = tenant.into();
        if tenant.len() > MAX_TENANT {
            return Err(Error::TenantTooLong { len: tenant.len() });
        }
        Ok(Self { front_end, tenant })
    }

    /// The front-end this namespace belongs to.
    #[must_use]
    pub fn front_end(&self) -> FrontEnd {
        self.front_end
    }

    /// The tenant within the front-end.
    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Derives the store key for `client_key` in this namespace.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyTooLong`] when `client_key` is longer than
    /// [`MAX_CLIENT_KEY`] bytes.
    pub fn key(&self, client_key: &[u8]) -> Result<Key> {
        if client_key.len() > MAX_CLIENT_KEY {
            return Err(Error::KeyTooLong {
                len: client_key.len(),
            });
        }
        let mut hasher = Hasher::new();
        hasher
            .field(KEY_DOMAIN)
            .field(self.front_end.as_str().as_bytes())
            .field(self.tenant.as_bytes())
            .field(client_key);
        Ok(Key(hasher.finish()))
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.front_end, self.tenant)
    }
}

/// The fixed-size name of one entry in the store.
///
/// Its text form is 64 lowercase hexadecimal digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(Digest);

impl Key {
    /// The digest this key is.
    #[must_use]
    pub fn digest(&self) -> Digest {
        self.0
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod test;
