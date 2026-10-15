//! Who may read and write which namespace.
//!
//! A token file grants each token one or more scopes:
//!
//! ```text
//! # token          scopes
//! ci-writer-token  *:write
//! dev-read-token   turbo/my-team:read  nx:read
//! ```
//!
//! A scope is `<front-end>[/<tenant>]:<read|write>`, where `*` (or an
//! omitted tenant) matches any. `write` includes `read`. Clients present a
//! token as `Authorization: Bearer <token>` (Turbo, Nx) or as the password
//! of HTTP basic auth (Gradle, Bazel, REAPI); the user name is ignored.
//!
//! Without a token file every request is allowed, which the server accepts
//! only on a loopback address.

use std::path::Path;

use base64::Engine as _;
use http::HeaderMap;
use tinymold_store::{FrontEnd, Namespace};

use crate::{Error, Result};

/// What a request needs to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    /// Look up and download.
    Read,
    /// Upload. Implies [`Permission::Read`].
    Write,
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// No credential, or one the server does not know: HTTP 401.
    Unauthenticated,
    /// A known credential without the needed scope: HTTP 403.
    Forbidden,
}

/// One grant of the token file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    front_end: Option<FrontEnd>,
    tenant: Option<String>,
    permission: Permission,
}

impl Scope {
    fn allows(&self, namespace: &Namespace, need: Permission) -> bool {
        self.permission >= need
            && self
                .front_end
                .is_none_or(|front| front == namespace.front_end())
            && self
                .tenant
                .as_deref()
                .is_none_or(|tenant| tenant == namespace.tenant())
    }
}

/// The parsed token file.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    grants: Vec<(String, Vec<Scope>)>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print a token, not even in a debug dump.
        f.debug_struct("Tokens")
            .field("grants", &self.grants.len())
            .finish()
    }
}

impl Tokens {
    /// Reads and parses the token file at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ReadFile`] when it cannot be read and
    /// [`Error::TokenFile`] when a line does not parse.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text)
    }

    /// Parses token file text. Blank lines and lines starting with `#` are
    /// skipped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TokenFile`] for the first line that does not parse.
    pub fn parse(text: &str) -> Result<Self> {
        let mut grants = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line_number = index + 1;
            let fail = |reason| Error::TokenFile {
                line: line_number,
                reason,
            };
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut words = line.split_whitespace();
            let token = words.next().ok_or_else(|| fail("missing token"))?;
            let scopes = words
                .map(|word| parse_scope(word).map_err(fail))
                .collect::<Result<Vec<_>>>()?;
            if scopes.is_empty() {
                return Err(fail("a token needs at least one scope"));
            }
            grants.push((token.to_owned(), scopes));
        }
        Ok(Self { grants })
    }

    fn scopes(&self, credential: &str) -> Option<&[Scope]> {
        // Compare against every token so the time taken does not say which
        // one a guess was close to.
        let mut found = None;
        for (token, scopes) in &self.grants {
            if constant_time_eq(token.as_bytes(), credential.as_bytes()) && found.is_none() {
                found = Some(scopes.as_slice());
            }
        }
        found
    }
}

fn parse_scope(word: &str) -> std::result::Result<Scope, &'static str> {
    let (target, permission) = word
        .rsplit_once(':')
        .ok_or("a scope ends in :read or :write")?;
    let permission = match permission {
        "read" => Permission::Read,
        "write" => Permission::Write,
        _ => return Err("unknown permission"),
    };
    let (front, tenant) = match target.split_once('/') {
        Some((front, tenant)) => (front, Some(tenant)),
        None => (target, None),
    };
    let front_end = match front {
        "*" => None,
        name => Some(
            FrontEnd::ALL
                .into_iter()
                .find(|front| front.as_str() == name)
                .ok_or("unknown front-end")?,
        ),
    };
    let tenant = tenant.filter(|tenant| *tenant != "*").map(str::to_owned);
    Ok(Scope {
        front_end,
        tenant,
        permission,
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The server's access policy.
#[derive(Debug, Clone)]
pub struct Access {
    tokens: Option<Tokens>,
    anonymous_reads: bool,
}

impl Access {
    /// A policy that allows every request: no token file.
    #[must_use]
    pub fn open() -> Self {
        Self {
            tokens: None,
            anonymous_reads: true,
        }
    }

    /// A policy that grants what `tokens` lists, and reads to anyone when
    /// `anonymous_reads` is set.
    #[must_use]
    pub fn new(tokens: Tokens, anonymous_reads: bool) -> Self {
        Self {
            tokens: Some(tokens),
            anonymous_reads,
        }
    }

    /// Whether this policy lets anyone do anything.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.tokens.is_none()
    }

    /// Decides whether a request with `headers` may do `need` in
    /// `namespace`.
    ///
    /// # Errors
    ///
    /// Returns [`Denied::Unauthenticated`] when the request carries no known
    /// credential, and [`Denied::Forbidden`] when its credential lacks the
    /// scope.
    pub fn check(
        &self,
        headers: &HeaderMap,
        namespace: &Namespace,
        need: Permission,
    ) -> std::result::Result<(), Denied> {
        let Some(tokens) = &self.tokens else {
            return Ok(());
        };
        let Some(credential) = credential(headers) else {
            return if need == Permission::Read && self.anonymous_reads {
                Ok(())
            } else {
                Err(Denied::Unauthenticated)
            };
        };
        match tokens.scopes(&credential) {
            None => Err(Denied::Unauthenticated),
            Some(scopes) if scopes.iter().any(|scope| scope.allows(namespace, need)) => Ok(()),
            // A known token without the scope may still read anonymously.
            Some(_) if need == Permission::Read && self.anonymous_reads => Ok(()),
            Some(_) => Err(Denied::Forbidden),
        }
    }
}

/// The token a request presents: a bearer token, or the password of basic
/// auth.
fn credential(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(http::header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = value.trim().split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        return Some(rest.trim().to_owned());
    }
    if scheme.eq_ignore_ascii_case("basic") {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(rest.trim())
            .ok()?;
        let text = String::from_utf8(decoded).ok()?;
        let (_user, password) = text.split_once(':')?;
        return Some(password.to_owned());
    }
    None
}

#[cfg(test)]
mod test;
