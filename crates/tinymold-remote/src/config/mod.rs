//! Where the remote cache is and how to reach it, from the environment.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `TINYMOLD_REMOTE_URL` | `grpc://host:port` (plaintext) or `grpcs://host[:port]` (TLS, port 443 by default) |
//! | `TINYMOLD_REMOTE_AUTH` | `user:password`, sent as HTTP basic auth |
//! | `TINYMOLD_REMOTE_INSTANCE` | the REAPI instance name; empty by default |
//! | `TINYMOLD_REMOTE_TIMEOUT` | per-call timeout, such as `5s` or `500ms`; 5 s by default |

use std::ffi::OsString;
use std::fmt;
use std::time::Duration;

use crate::{Error, Result};

/// How long one call may take by default.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// How to reach a remote cache.
#[derive(Clone, PartialEq, Eq)]
pub struct RemoteConfig {
    /// The server, as an `http://` or `https://` URI for the gRPC channel.
    pub endpoint: String,
    /// The `authorization` header value, `Basic <base64>`, when a credential
    /// is configured.
    pub authorization: Option<String>,
    /// The REAPI instance name.
    pub instance: String,
    /// How long one call may take.
    pub timeout: Duration,
}

impl fmt::Debug for RemoteConfig {
    // The credential never appears in logs or panics.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteConfig")
            .field("endpoint", &self.endpoint)
            .field("authorization", &self.authorization.as_ref().map(|_| "…"))
            .field("instance", &self.instance)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl RemoteConfig {
    /// A configuration for the server at `url`, anonymous, with the default
    /// instance and timeout.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidUrl`] when `url` is not a gRPC or HTTP URL with
    /// a host.
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self {
            endpoint: endpoint(url)?,
            authorization: None,
            instance: String::new(),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Reads the configuration from the `TINYMOLD_REMOTE_*` variables through
    /// `lookup`, or `None` when no URL is set.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidUrl`], [`Error::InvalidAuth`] or
    /// [`Error::InvalidTimeout`] when a variable is set but malformed.
    pub fn from_env(lookup: impl Fn(&str) -> Option<OsString>) -> Result<Option<Self>> {
        let text = |name: &str| {
            lookup(name)
                .map(|value| value.to_string_lossy().trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let Some(url) = text("TINYMOLD_REMOTE_URL") else {
            return Ok(None);
        };
        let mut config = Self::new(&url)?;
        if let Some(credential) = text("TINYMOLD_REMOTE_AUTH") {
            config.authorization = Some(basic_auth(&credential)?);
        }
        if let Some(instance) = text("TINYMOLD_REMOTE_INSTANCE") {
            config.instance = instance;
        }
        if let Some(timeout) = text("TINYMOLD_REMOTE_TIMEOUT") {
            config.timeout = parse_timeout(&timeout)?;
        }
        Ok(Some(config))
    }

    /// Whether the channel uses TLS.
    #[must_use]
    pub fn uses_tls(&self) -> bool {
        self.endpoint.starts_with("https://")
    }
}

/// The channel URI for a remote URL.
fn endpoint(url: &str) -> Result<String> {
    let invalid = || Error::InvalidUrl(url.to_owned());
    let (scheme, rest) = url.split_once("://").ok_or_else(invalid)?;
    let (tls, default_port) = match scheme {
        "grpc" | "http" => (false, None),
        "grpcs" | "https" => (true, Some(443)),
        _ => return Err(invalid()),
    };
    let authority = rest.trim_end_matches('/');
    if authority.is_empty() || authority.contains('/') || authority.contains('@') {
        return Err(invalid());
    }
    let has_port = authority
        .rsplit_once(':')
        .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
    let authority = match (has_port, default_port) {
        (true, _) => authority.to_owned(),
        (false, Some(port)) => format!("{authority}:{port}"),
        (false, None) => return Err(invalid()),
    };
    let scheme = if tls { "https" } else { "http" };
    Ok(format!("{scheme}://{authority}"))
}

/// `Basic <base64(user:password)>`.
fn basic_auth(credential: &str) -> Result<String> {
    match credential.split_once(':') {
        Some((user, _)) if !user.is_empty() => {
            Ok(format!("Basic {}", base64(credential.as_bytes())))
        }
        _ => Err(Error::InvalidAuth),
    }
}

/// Parses `5s`, `500ms`, or a plain number of seconds.
fn parse_timeout(text: &str) -> Result<Duration> {
    let invalid = || Error::InvalidTimeout(text.to_owned());
    let (number, millis) = if let Some(number) = text.strip_suffix("ms") {
        (number, true)
    } else {
        (text.strip_suffix('s').unwrap_or(text), false)
    };
    let value: u64 = number.parse().map_err(|_| invalid())?;
    if value == 0 {
        return Err(invalid());
    }
    Ok(if millis {
        Duration::from_millis(value)
    } else {
        Duration::from_secs(value)
    })
}

/// Standard base64 with padding (RFC 4648 §4).
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = chunk.iter().enumerate().fold(0u32, |acc, (index, byte)| {
            acc | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (triple >> (18 - 6 * index)) & 0x3f;
                text.push(char::from(ALPHABET[sextet as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

#[cfg(test)]
mod test;
