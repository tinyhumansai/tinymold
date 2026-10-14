//! Classifying a rustc command line and picking out what the key needs.
//!
//! rustc accepts most flags in several spellings (`--emit x`, `--emit=x`,
//! `-C x`, `-Cx`), so each is reduced to one canonical text before it reaches
//! the key: the same compile spelled two ways must get the same key.
//!
//! Anything this parser does not fully understand is passed through unchanged
//! rather than guessed at: stdin input, `@argfiles`, `--print`, an explicit
//! `-o`, `--emit` to explicit paths, and more than one input file. A pass-through
//! is only a missed hit; a wrong guess could be a wrong one.

mod types;

use std::path::PathBuf;

pub(crate) use types::KeyArg;
pub use types::{Classified, Extern, Invocation, SearchPath};

/// Long flags that take a value, as `--flag value` or `--flag=value`.
const LONG_WITH_VALUE: &[&str] = &[
    "--crate-name",
    "--edition",
    "--error-format",
    "--json",
    "--crate-type",
    "--emit",
    "--cfg",
    "--check-cfg",
    "--out-dir",
    "--extern",
    "--cap-lints",
    "--target",
    "--allow",
    "--warn",
    "--deny",
    "--forbid",
    "--force-warn",
    "--remap-path-prefix",
    "--diagnostic-width",
    "--sysroot",
    "--codegen",
    "--color",
    "--env-set",
    "--explain",
    "--print",
];

/// Short flags that take a value, as `-X value` or `-Xvalue`.
const SHORT_WITH_VALUE: &[&str] = &["-C", "-L", "-l", "-A", "-W", "-D", "-F", "-Z", "-o"];

/// Command lines that only ask rustc about itself.
const QUERIES: &[&str] = &["-V", "-vV", "--version", "-h", "--help"];

impl Invocation {
    /// Parses a rustc command line, without the program name.
    #[must_use]
    pub fn parse<I, S>(args: I) -> Classified
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let args: Vec<String> = args.into_iter().map(Into::into).collect();
        match Parser::default().run(&args) {
            Ok(invocation) => Classified::Cacheable(invocation),
            Err(reason) => Classified::PassThrough(reason),
        }
    }

    /// Whether this invocation runs the linker: it emits `link` and builds a
    /// binary, test, dylib, cdylib or proc macro.
    #[must_use]
    pub fn links(&self) -> bool {
        let linked = ["bin", "dylib", "cdylib", "proc-macro"];
        self.emit.iter().any(|kind| kind == "link")
            && (self.test
                || self.crate_types.is_empty()
                || self
                    .crate_types
                    .iter()
                    .any(|kind| linked.contains(&kind.as_str())))
    }

    /// The name prefixes every output of this invocation starts with:
    /// `<crate><extra>` and `lib<crate><extra>`. Cargo makes `extra` unique
    /// per unit, so a file in the out-dir with one of these prefixes belongs
    /// to this unit.
    #[must_use]
    pub fn output_prefixes(&self) -> [String; 2] {
        let stem = format!("{}{}", self.crate_name, self.extra_filename);
        [format!("lib{stem}"), stem]
    }
}

#[derive(Default)]
struct Parser {
    crate_name: Option<String>,
    crate_types: Vec<String>,
    emit: Vec<String>,
    out_dir: Option<PathBuf>,
    extra_filename: String,
    source: Option<PathBuf>,
    externs: Vec<Extern>,
    search_paths: Vec<SearchPath>,
    linker: Option<PathBuf>,
    test: bool,
    key_args: Vec<KeyArg>,
}

impl Parser {
    fn run(mut self, args: &[String]) -> Result<Invocation, &'static str> {
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            if QUERIES.contains(&arg.as_str()) {
                return Err("asks rustc about itself");
            }
            if arg == "-" {
                return Err("reads its input from stdin");
            }
            if arg.starts_with('@') {
                return Err("reads arguments from a file");
            }
            if let Some(long) = arg.strip_prefix("--") {
                let (name, inline) = match long.split_once('=') {
                    Some((name, value)) => (format!("--{name}"), Some(value.to_owned())),
                    None => (arg.clone(), None),
                };
                if LONG_WITH_VALUE.contains(&name.as_str()) {
                    let value = inline
                        .or_else(|| rest.next().cloned())
                        .ok_or("a flag has no value")?;
                    self.long(&name, value)?;
                } else {
                    if name == "--test" {
                        self.test = true;
                    }
                    self.text(arg.clone());
                }
            } else if arg.len() > 1 && arg.starts_with('-') {
                // `get` rather than slicing: a non-ASCII second character is
                // not a known flag, and must not panic.
                let flag = arg.get(..2).unwrap_or_default();
                if SHORT_WITH_VALUE.contains(&flag) {
                    let value = if arg.len() > 2 {
                        arg[2..].to_owned()
                    } else {
                        rest.next().cloned().ok_or("a flag has no value")?
                    };
                    self.short(flag, value)?;
                } else {
                    self.text(arg.clone());
                }
            } else if self.source.replace(PathBuf::from(arg)).is_some() {
                return Err("has more than one input file");
            }
        }
        if self.emit.is_empty() {
            return Err("has no --emit");
        }
        Ok(Invocation {
            crate_name: self.crate_name.ok_or("has no --crate-name")?,
            crate_types: self.crate_types,
            emit: self.emit,
            out_dir: self.out_dir.ok_or("has no --out-dir")?,
            extra_filename: self.extra_filename,
            source: self.source.ok_or("has no input file")?,
            externs: self.externs,
            search_paths: self.search_paths,
            linker: self.linker,
            test: self.test,
            key_args: self.key_args,
        })
    }

    fn text(&mut self, text: String) {
        self.key_args.push(KeyArg::Text(text));
    }

    fn long(&mut self, name: &str, value: String) -> Result<(), &'static str> {
        match name {
            "--print" => return Err("prints compiler information"),
            "--crate-name" => self.crate_name = Some(value.clone()),
            "--crate-type" => self.crate_types.extend(value.split(',').map(str::to_owned)),
            "--emit" => {
                if value.contains('=') {
                    return Err("emits to an explicit path");
                }
                self.emit.extend(value.split(',').map(str::to_owned));
            }
            "--out-dir" => {
                self.out_dir = Some(PathBuf::from(value));
                return Ok(());
            }
            "--extern" => {
                let (name, path) = match value.split_once('=') {
                    Some((name, path)) => (name.to_owned(), Some(PathBuf::from(path))),
                    None => (value.clone(), None),
                };
                self.key_args.push(KeyArg::Extern(self.externs.len()));
                self.externs.push(Extern { name, path });
                return Ok(());
            }
            "--codegen" => return self.codegen(value),
            _ => {}
        }
        self.text(format!("{name}={value}"));
        Ok(())
    }

    fn short(&mut self, flag: &str, value: String) -> Result<(), &'static str> {
        match flag {
            "-o" => Err("writes to an explicit output path"),
            "-C" => self.codegen(value),
            "-L" => {
                let (kind, path) = match value.split_once('=') {
                    Some((kind, path)) => (kind.to_owned(), path.to_owned()),
                    None => ("all".to_owned(), value),
                };
                if kind != "dependency" {
                    self.key_args.push(KeyArg::Search(self.search_paths.len()));
                    self.search_paths.push(SearchPath {
                        kind,
                        path: PathBuf::from(path),
                    });
                }
                Ok(())
            }
            _ => {
                self.text(format!("{flag} {value}"));
                Ok(())
            }
        }
    }

    fn codegen(&mut self, value: String) -> Result<(), &'static str> {
        let (key, setting) = match value.split_once('=') {
            Some((key, setting)) => (key, Some(setting)),
            None => (value.as_str(), None),
        };
        match (key, setting) {
            // Where incremental state lives is not an input; whether the
            // compile is incremental is kept, since it changes output bytes.
            ("incremental", _) => {
                self.text("-C incremental".to_owned());
                return Ok(());
            }
            ("extra-filename", Some(extra)) => self.extra_filename = extra.to_owned(),
            ("linker", Some(linker)) => self.linker = Some(PathBuf::from(linker)),
            _ => {}
        }
        self.text(format!("-C {value}"));
        Ok(())
    }
}

#[cfg(test)]
mod test;
