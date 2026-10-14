//! Telling a `RUSTC_WRAPPER` call apart from a tinymold command.
//!
//! With `RUSTC_WRAPPER=tinymold`, cargo runs `tinymold <compiler> <args…>`.
//! A first argument that names a program, by containing a path separator or
//! being called `rustc` or `clippy-driver`, makes the call a wrapper call;
//! anything else (a subcommand, a flag) is a normal command line. The
//! subcommand names never contain a separator, so the two cannot collide.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use tinymold_wrap::Config;

/// Compilers recognized by bare name; any path is recognized by its
/// separator.
const COMPILERS: &[&str] = &["rustc", "clippy-driver"];

/// The compiler and its arguments when `args` (without the program name) is a
/// wrapper call, or `None` for a normal tinymold command line.
#[must_use]
pub fn wrapper_call(args: &[OsString]) -> Option<(&Path, &[OsString])> {
    let (first, rest) = args.split_first()?;
    is_program(first).then(|| (Path::new(first), rest))
}

fn is_program(arg: &OsStr) -> bool {
    let text = arg.to_string_lossy();
    if text.starts_with('-') {
        return false;
    }
    let path = Path::new(arg);
    path.components().count() > 1
        || path
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(|stem| COMPILERS.contains(&stem))
}

/// The wrapper configuration from the environment: the default store and,
/// when `TINYMOLD_LOG` is set, a log file. `None` when no store location can
/// be derived, in which case the compiler runs uncached.
#[must_use]
pub fn wrapper_config(lookup: impl Fn(&str) -> Option<OsString>) -> Option<Config> {
    let store = crate::default_store(&lookup)?;
    let mut config = Config::new(store);
    config.log = lookup("TINYMOLD_LOG")
        .filter(|value| !value.is_empty())
        .map(Into::into);
    Some(config)
}

#[cfg(test)]
mod test;
