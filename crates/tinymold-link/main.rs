//! The `tinymold-link` executable, forked from mold's: runs the ELF
//! linker, or the Mach-O linker if invoked as `ld64` or `ld64.<anything>`
//! (such as `ld64.tinymold`), instantiated for the target the inputs are for.
//! Each target is instantiated in a crate of its own so that the compiler
//! can build them in parallel, and a feature per target decides which of
//! them are built in.

// rustc emits asynchronous unwind tables, which compact unwind can't
// encode, so the unwind info of most functions goes to __eh_frame. In a
// debug build, that section exceeds the 16 MiB that compact unwind can
// refer to, and Apple's ld warns about that. The warning concerns only
// the speed of unwinding, so we keep rustc from reporting it.
#![cfg_attr(all(target_vendor = "apple", debug_assertions), allow(linker_messages))]

use mold_elf::driver::Cmdline;
use std::ffi::OsStr;
use std::path::Path;

// A Rust executable can define only one global allocator, so select mimalloc
// here rather than in the linker library.
#[cfg(not(feature = "system-allocator"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

type LinkFn = fn(Cmdline) -> Option<&'static str>;
type MachoLinkFn = fn(mold_macho::driver::ParsedArgs);

// Each target has its own monomorphized link function. The ELF linker
// starts with the first enabled target and switches to the matching
// function if the inputs are for another target. The Mach-O linker finds
// the target as it parses the options, and calls its function.
const ELF_TARGETS: &[(&str, LinkFn)] = &[
    #[cfg(feature = "x86_64")]
    ("x86_64", mold_elf_x86_64::link),
    #[cfg(feature = "arm64")]
    ("arm64", mold_elf_arm64::link),
];

const MACHO_TARGETS: &[(&str, MachoLinkFn)] = &[
    #[cfg(feature = "macho-arm64")]
    ("arm64", mold_macho_arm64::link),
    #[cfg(feature = "macho-x86_64")]
    ("x86_64", mold_macho_x86_64::link),
];

fn link_fn<F: Copy>(targets: &[(&str, F)], target: &str) -> F {
    for &(name, link) in targets {
        if name == target {
            return link;
        }
    }
    eprintln!(
        "tinymold-link: unsupported target: {target}; rebuild tinymold-link with the appropriate target support"
    );
    std::process::exit(1);
}

// As with lld's ld64.lld, the name tells mold to act as Apple's linker,
// ld64. clang on macOS looks for ld64.mold when given -fuse-ld=mold.
fn is_ld64(argv0: &OsStr) -> bool {
    let name = Path::new(argv0).file_name().unwrap_or_default().as_encoded_bytes();
    name == b"ld64" || name.starts_with(b"ld64.")
}

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.first().is_some_and(|argv0| is_ld64(argv0)) {
        let link = |target: &str, parsed| link_fn(MACHO_TARGETS, target)(parsed);
        mold_macho::driver::main(args, link);
        std::process::exit(0);
    }

    let Some(&(initial_target, _)) = ELF_TARGETS.first() else {
        eprintln!(
            "tinymold-link: no targets enabled; rebuild tinymold-link with the appropriate target support"
        );
        std::process::exit(1);
    };
    let link = |target: &str, cmdline| link_fn(ELF_TARGETS, target)(cmdline);
    mold_elf::driver::main(args, initial_target, link);
    std::process::exit(0);
}
