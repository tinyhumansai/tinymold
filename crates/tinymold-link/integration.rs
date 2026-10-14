//! Runs mold's shell tests against `tinymold-link`, for the format the host
//! builds programs for. The Mach-O tests drive Apple's toolchain, which only
//! macOS has, and the ELF tests need a toolchain that makes ELF programs,
//! which macOS lacks.
//!
//! The suite takes minutes and needs compilers, binutils and (for cross
//! targets) QEMU, so it runs only when `TINYMOLD_LINK_SHELL_TESTS` is set,
//! for example `TINYMOLD_LINK_SHELL_TESTS=1 cargo test -p tinymold-link`.
//! Its own options follow `--`; see `tinymold-link-tests/lib.rs`.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::var_os("TINYMOLD_LINK_SHELL_TESTS").is_none() {
        eprintln!("tinymold-link: shell tests skipped; set TINYMOLD_LINK_SHELL_TESTS=1 to run them");
        return ExitCode::SUCCESS;
    }

    #[cfg(target_os = "macos")]
    let (cases, run) = ("tinymold-link-tests/macho", mold_tests::macho::run);
    #[cfg(not(target_os = "macos"))]
    let (cases, run) = ("tinymold-link-tests/elf", mold_tests::elf::run);

    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    run(&crates.join(cases), Path::new(env!("CARGO_BIN_EXE_tinymold-link")))
}
