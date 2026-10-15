//! Telling from a compile's source text whether an env var only picks the
//! files it includes.
//!
//! A build script's `OUT_DIR` differs in every worktree, so keying a compile
//! by its raw value gives every worktree a key of its own. The value can be
//! keyed without its root only when it cannot change the output except
//! through the files it names, which the key covers by content. The one use
//! shown to be that is the generated-code idiom:
//!
//! ```text
//! include!(concat!(env!("OUT_DIR"), "/generated.rs"));
//! ```
//!
//! with `include_str!` or `include_bytes!` in place of `include!`. Anything
//! else, such as `env!("OUT_DIR").len()`, may put something derived from the
//! path into the output without putting the path itself there, so the
//! caller's scan of the outputs cannot catch it.
//!
//! The check is textual, so it is deliberately strict: every mention of the
//! name outside a comment line must be one of those includes, and no `env!`
//! may take anything but a plain string literal, since the name could then
//! be spelled another way (`concat!("OUT", "_DIR")`, an escape, a macro
//! argument). A crate that redefines `include!`, `concat!` or `env!` with
//! `macro_rules!` is rejected too. What text cannot show is a macro from
//! another crate expanding a transformed use into this one; that residual
//! risk is described in `docs/specs/rustc-wrapper.md`.

/// How the source of one compile uses one env var.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnvUse {
    /// `include*!(concat!(env!("NAME"), "..."))` invocations.
    pub include_paths: usize,
    /// Mentions of the name outside a comment line, the includes among
    /// them.
    pub mentions: usize,
    /// Whether the source can name the variable in a way the text does not
    /// show: an `env!` whose argument is not a plain string literal, or a
    /// `macro_rules!` redefining one of the macros the idiom uses.
    pub opaque: bool,
}

/// The macros the include idiom relies on, which a source must not redefine.
const IDIOM_MACROS: &[&[u8]] = &[b"include", b"include_str", b"include_bytes", b"concat", b"env"];

impl EnvUse {
    /// Scans one source file for the uses of the env var `name`.
    #[must_use]
    pub fn scan(source: &[u8], name: &str) -> Self {
        let code = without_comment_lines(source);
        let name = name.as_bytes();
        let mut found = Self::default();
        for at in 0..code.len() {
            let rest = &code[at..];
            let starts_word = at == 0 || !is_ident(code[at - 1]);
            if starts_word && rest.starts_with(name) && !rest.get(name.len()).is_some_and(|&b| is_ident(b)) {
                found.mentions += 1;
            }
            if starts_word && include_path(rest, name) {
                found.include_paths += 1;
            }
            if rest.starts_with(b"env") && !env_argument_is_plain(&rest[3..]) {
                found.opaque = true;
            }
            if starts_word && redefines_idiom_macro(rest) {
                found.opaque = true;
            }
        }
        found
    }

    /// The uses of `self` and `other` together, as in two files of one
    /// compile.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            include_paths: self.include_paths + other.include_paths,
            mentions: self.mentions + other.mentions,
            opaque: self.opaque || other.opaque,
        }
    }

    /// Whether the variable only picks files to include: it is used at least
    /// once, every use is the include idiom, and nothing hides another use.
    #[must_use]
    pub fn only_picks_included_files(&self) -> bool {
        self.include_paths > 0 && self.mentions == self.include_paths && !self.opaque
    }
}

/// `source` with every line whose first non-blank characters are `//`
/// blanked. Such a line is never code: outside a literal or block comment it
/// is a comment, and inside one it is text.
fn without_comment_lines(source: &[u8]) -> Vec<u8> {
    let mut code = Vec::with_capacity(source.len());
    for line in source.split_inclusive(|&b| b == b'\n') {
        if line.trim_ascii_start().starts_with(b"//") {
            code.push(b'\n');
        } else {
            code.extend_from_slice(line);
        }
    }
    code
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// A cursor over source text, skipping whitespace between tokens.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn blank(&mut self) {
        self.0 = self.0.trim_ascii_start();
    }

    /// Consumes `token` after any whitespace.
    fn eat(&mut self, token: &[u8]) -> bool {
        self.blank();
        match self.0.strip_prefix(token) {
            Some(rest) => {
                self.0 = rest;
                true
            }
            None => false,
        }
    }

    /// Consumes a `"..."` literal without escapes and returns its contents.
    fn plain_literal(&mut self) -> Option<&'a [u8]> {
        self.blank();
        let body = self.0.strip_prefix(b"\"")?;
        let end = body.iter().position(|&b| b == b'"' || b == b'\\')?;
        if body[end] != b'"' {
            return None;
        }
        self.0 = &body[end + 1..];
        Some(&body[..end])
    }

    /// Consumes a `"..."` literal, escapes allowed.
    fn literal(&mut self) -> bool {
        self.blank();
        let Some(mut body) = self.0.strip_prefix(b"\"") else {
            return false;
        };
        loop {
            match body {
                [b'"', rest @ ..] => {
                    self.0 = rest;
                    return true;
                }
                [b'\\', _, rest @ ..] | [_, rest @ ..] => body = rest,
                [] => return false,
            }
        }
    }
}

/// Whether `text` starts with `include*!(concat!(env!("NAME"), "...", ...))`.
fn include_path(text: &[u8], name: &[u8]) -> bool {
    let mut at = Cursor(text);
    let Some(rest) = text.strip_prefix(b"include") else {
        return false;
    };
    at.0 = rest
        .strip_prefix(b"_bytes")
        .or_else(|| rest.strip_prefix(b"_str"))
        .unwrap_or(rest);
    if !(at.eat(b"!") && at.eat(b"(") && at.eat(b"concat") && at.eat(b"!") && at.eat(b"(")) {
        return false;
    }
    if !(at.eat(b"env") && at.eat(b"!") && at.eat(b"(") && at.plain_literal() == Some(name) && at.eat(b")")) {
        return false;
    }
    let mut suffixes = 0;
    while at.eat(b",") {
        if !at.literal() {
            break;
        }
        suffixes += 1;
    }
    suffixes > 0 && at.eat(b")") && at.eat(b")")
}

/// Whether the text after an `env` is anything but an invocation whose
/// first argument is a plain string literal: `!(` (or `![`, `!{`), the
/// literal, then a `,` or the closing delimiter. Text that is not an
/// invocation at all (`std::env::var`, a word in a string) is plain.
fn env_argument_is_plain(after: &[u8]) -> bool {
    let mut at = Cursor(after);
    if !at.eat(b"!") {
        return true;
    }
    let close = if at.eat(b"(") {
        b")"
    } else if at.eat(b"[") {
        b"]"
    } else if at.eat(b"{") {
        b"}"
    } else {
        return true;
    };
    at.plain_literal().is_some() && (at.eat(b",") || at.eat(close))
}

/// Whether `text` starts a `macro_rules!` definition of a macro the idiom
/// uses.
fn redefines_idiom_macro(text: &[u8]) -> bool {
    let mut at = Cursor(text);
    if !(at.eat(b"macro_rules") && at.eat(b"!")) {
        return false;
    }
    at.blank();
    let len = at.0.iter().take_while(|&&b| is_ident(b)).count();
    IDIOM_MACROS.contains(&&at.0[..len])
}

#[cfg(test)]
mod test;
