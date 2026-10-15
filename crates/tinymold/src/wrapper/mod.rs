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

use crate::gc::ByteSize;

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
/// when `TINYMOLD_LOG` or `TINYMOLD_TRACE` is set, a log or trace file, and
/// with `TINYMOLD_MAX_EXECUTABLE_SIZE` (such as `256M`) the largest executable
/// output a miss stores. A malformed size stores every size: a cache setting
/// never fails the build. `None` when no store location can be derived, in
/// which case the compiler runs uncached.
#[must_use]
pub fn wrapper_config(lookup: impl Fn(&str) -> Option<OsString>) -> Option<Config> {
    let store = crate::default_store(&lookup)?;
    let mut config = Config::new(store);
    config.log = lookup("TINYMOLD_LOG")
        .filter(|value| !value.is_empty())
        .map(Into::into);
    config.trace = lookup("TINYMOLD_TRACE")
        .filter(|value| !value.is_empty())
        .map(Into::into);
    config.max_executable = lookup("TINYMOLD_MAX_EXECUTABLE_SIZE")
        .and_then(|value| value.to_str()?.parse::<ByteSize>().ok())
        .map(|size| size.0);
    Some(config)
}

/// The remote cache tier the wrapper consults on a local miss, from the
/// `TINYMOLD_REMOTE_*` variables. `None` when none is configured, when the
/// configuration is malformed (`tinymold remote check` reports why), or when
/// the binary was built without the `remote` feature.
#[must_use]
pub fn wrapper_remote(
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Option<Box<dyn tinymold_wrap::Remote>> {
    #[cfg(feature = "remote")]
    {
        let config = tinymold_remote::RemoteConfig::from_env(lookup).ok()??;
        let remote = tinymold_remote::RemoteCache::new(&config).ok()?;
        Some(Box::new(remote))
    }
    #[cfg(not(feature = "remote"))]
    {
        let _ = lookup;
        None
    }
}

#[cfg(test)]
mod test;
