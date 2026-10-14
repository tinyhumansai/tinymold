//! Reading what a compile used from rustc's dep-info (`--emit=dep-info`).
//!
//! rustc writes one Makefile-style file per crate:
//!
//! ```text
//! /target/debug/deps/demo-1a2b.d: src/lib.rs src/a.rs
//!
//! src/lib.rs:
//! src/a.rs:
//!
//! # env-dep:CARGO_PKG_VERSION=0.1.0
//! # env-dep:UNSET_BUT_READ
//! ```
//!
//! Every input file appears on a line of its own ending in `:`, with spaces
//! escaped as `\ `. Every environment variable the crate read through `env!`
//! or `option_env!` appears as an `env-dep` comment, without a value when it
//! was unset; rustc escapes `\`, newlines and carriage returns in values.

use crate::{Error, Result};

/// The files and environment a compile read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DepInfo {
    /// Input files, as rustc wrote them: relative to the compile's current
    /// directory, or absolute.
    pub inputs: Vec<String>,
    /// Environment variables read, with their values; `None` for a variable
    /// that was read while unset.
    pub env: Vec<(String, Option<String>)>,
}

impl DepInfo {
    /// Parses a dep-info file's text.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedDepInfo`] for an `env-dep` line without a
    /// variable name.
    pub fn parse(text: &str) -> Result<Self> {
        let mut info = Self::default();
        for (index, line) in text.lines().enumerate() {
            if let Some(dep) = line.strip_prefix("# env-dep:") {
                let (name, value) = match dep.split_once('=') {
                    Some((name, value)) => (name, Some(unescape_env(value))),
                    None => (dep, None),
                };
                if name.is_empty() {
                    return Err(Error::MalformedDepInfo { line: index + 1 });
                }
                info.env.push((name.to_owned(), value));
            } else if !line.starts_with('#') {
                if let Some(path) = line.strip_suffix(':').filter(|path| !path.is_empty()) {
                    info.inputs.push(path.replace("\\ ", " "));
                }
            }
        }
        Ok(info)
    }
}

/// Undoes rustc's escaping of env-dep values.
fn unescape_env(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod test;
