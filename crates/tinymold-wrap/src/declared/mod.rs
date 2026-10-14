//! Reading what a package declares about its tests, from its `Cargo.toml`.
//!
//! ```toml
//! [package.metadata.tinymold.test]
//! inputs = ["../fixtures", "../schema.sql"]  # read outside the package root
//! cache = false                              # never cache this package's tests
//! ```
//!
//! Each input is a path relative to the package root: a file, or a
//! directory standing for every file under it.
//!
//! This reads a strict subset of TOML rather than pulling a TOML parser into
//! every build: the table under exactly that header, `cache` as `true` or
//! `false`, and `inputs` as an array of plain double-quoted strings. A
//! misread table could leave out an input and replay a stale pass, so
//! anything else that mentions the table, another spelling of it, an
//! unknown key or an unsupported value, makes the package uncacheable.

/// The header of the table this module reads.
const HEADER: &str = "[package.metadata.tinymold.test]";

/// What a package declares about its tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Declared {
    /// Cacheable, with these inputs beyond the package tree.
    Inputs(Vec<String>),
    /// Never cached, for this reason.
    Uncached(&'static str),
}

/// What `manifest`, the text of a package's `Cargo.toml`, declares.
pub(crate) fn parse(manifest: &str) -> Declared {
    let mut inputs = Vec::new();
    let mut table = String::new();
    let mut lines = manifest.lines();
    while let Some(raw) = lines.next() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            table = line.to_owned();
            if table != HEADER && mentions_table(&table) {
                return Declared::Uncached("unreadable tinymold metadata");
            }
            continue;
        }
        if table != HEADER {
            if declares_elsewhere(&table, line) {
                return Declared::Uncached("unreadable tinymold metadata");
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Declared::Uncached("unreadable tinymold metadata");
        };
        match (key.trim(), strip_comment(value.trim())) {
            ("cache", "true") => {}
            ("cache", "false") => return Declared::Uncached("the package sets cache = false"),
            ("inputs", value) => {
                let mut array = value.to_owned();
                while !array.contains(']') {
                    let Some(next) = lines.next() else {
                        return Declared::Uncached("unreadable tinymold metadata");
                    };
                    array.push('\n');
                    array.push_str(next);
                }
                match parse_array(&array) {
                    Some(paths) if paths.iter().all(|path| is_relative(path)) => {
                        inputs.extend(paths);
                    }
                    _ => return Declared::Uncached("unreadable tinymold metadata"),
                }
            }
            _ => return Declared::Uncached("unreadable tinymold metadata"),
        }
    }
    Declared::Inputs(inputs)
}

/// Whether a table header other than [`HEADER`] could hold the same keys.
fn mentions_table(header: &str) -> bool {
    let name: String = header.chars().filter(|c| !c.is_whitespace()).collect();
    name.contains("metadata.tinymold") || name == "[package.metadata.tinymold.test]"
}

/// Whether a key line in `table` sets tinymold metadata through a dotted key
/// or an inline table.
fn declares_elsewhere(table: &str, line: &str) -> bool {
    let key = line.split('=').next().unwrap_or("").trim();
    match table {
        "[package.metadata]" => key.starts_with("tinymold"),
        "[package]" => key.starts_with("metadata") && line.contains("tinymold"),
        _ => false,
    }
}

/// `value` without a trailing `# comment`, for values that hold no strings.
fn strip_comment(value: &str) -> &str {
    if value.starts_with('[') {
        return value;
    }
    value.split('#').next().unwrap_or("").trim()
}

/// The strings of a `["a", "b"]` array, possibly spread over lines with
/// comments, or `None` for anything else.
fn parse_array(text: &str) -> Option<Vec<String>> {
    let body = text.trim().strip_prefix('[')?;
    let (items, rest) = body.split_once(']')?;
    let rest = rest.trim();
    if !(rest.is_empty() || rest.starts_with('#')) {
        return None;
    }
    let mut paths = Vec::new();
    let mut expect_item = true;
    let mut chars = items.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            '#' => {
                // A comment runs to the end of its line.
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' if expect_item => {
                let mut path = String::new();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' | '\n' => return None,
                        c => path.push(c),
                    }
                }
                paths.push(path);
                expect_item = false;
            }
            ',' if !expect_item => expect_item = true,
            _ => return None,
        }
    }
    Some(paths)
}

/// Whether `path` is relative: an absolute path names one machine's files.
fn is_relative(path: &str) -> bool {
    !path.is_empty() && !std::path::Path::new(path).has_root() && !path.contains(':')
}

#[cfg(test)]
mod test;
