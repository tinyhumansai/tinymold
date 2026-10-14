# tinymold-link: fork record

`tinymold-link` and its `tinymold-link-*` crates are a fork of
[mold](https://github.com/rui314/mold), the linker by Rui Ueyama, taken at
commit [`c522789`](https://github.com/rui314/mold/commit/c522789) (2026-10-11,
"Mark the Mach-O build ID as a version-3 UUID"), which is mold 3.0.0. The
reasons are in
[ADR 0009](../../docs/adr/0009-fork-mold-into-a-stateful-linker.md).

## License

mold is MIT-licensed. Every forked crate keeps that license (`license = "MIT"`
in its manifest) and carries mold's `LICENSE` file, although the rest of the
tinymold workspace is GPL-3.0-only. Code written for the fork inside these
crates is MIT too, so it can go back upstream.

## Crate mapping

| Upstream directory | Upstream package | Fork package |
|---|---|---|
| `common/` | `mold-common` | `tinymold-link-common` |
| `elf/` | `mold-elf` | `tinymold-link-elf` |
| `macho/` | `mold-macho` | `tinymold-link-macho` |
| `arch/elf-x86_64/` | `mold-elf-x86_64` | `tinymold-link-elf-x86_64` |
| `arch/elf-arm64/` | `mold-elf-arm64` | `tinymold-link-elf-arm64` |
| `arch/macho-arm64/` | `mold-macho-arm64` | `tinymold-link-macho-arm64` |
| `arch/macho-x86_64/` | `mold-macho-x86_64` | `tinymold-link-macho-x86_64` |
| `cli/` | `mold` | `tinymold-link` |
| `tests/` (runner, `elf/`, `macho/`) | `mold-tests` | `tinymold-link-tests` |

The root `Cargo.toml` declares each crate under its upstream name with
`package = "tinymold-link-…"`, so the sources still say `mold_common::`,
`mold_elf::` and so on. Keep it that way: it is what lets upstream commits
apply to this tree with `git apply --directory` and few conflicts.

Not imported: the eighteen other ELF arch crates (i386, arm32, riscv, ppc,
s390x, sparc64, m68k, sh4, loongarch and their big-endian variants), the
man page and docs, `dist.sh` and the install scripts. The generic ELF code for
those targets is still in `tinymold-link-elf/arch/`; only the crates that
instantiate it are gone.

## Local changes

Keep this list current. Each entry says what changed and why, so that a merge
from upstream knows what to preserve.

1. **Manifests.** Packages renamed as above, with `version` inherited from the
   workspace, `rust-version = "1.95"` and `license = "MIT"` set per crate.
   `tinymold-link` builds the `tinymold-link` binary and has four target
   features (`x86_64`, `arm64`, `macho-arm64`, `macho-x86_64`) instead of
   twenty-two.
2. **`tinymold-link/main.rs`.** The target table lists only those four
   targets; the messages say `tinymold-link`.
3. **Version banners** (`tinymold-link-common/lib.rs` `MOLD_VERSION`,
   `tinymold-link-elf/cmdline.rs` `VERSION`, `tinymold-link-macho/cmdline.rs`
   `VERSION_BANNER`, `tinymold-link-macho/mapfile.rs`). They start the way
   mold's do, with `mold 3.0.0` and `mold-macho 3.0.0`, because build tools
   and mold's own tests recognize the linker by that prefix, and then name the
   fork: `mold 3.0.0 (tinymold-link 0.0.1; compatible with GNU ld)`. The same
   string goes in `.comment`.
4. **`tinymold-link/integration.rs`.** The shell suite runs only when
   `TINYMOLD_LINK_SHELL_TESTS` is set, and finds its scripts under
   `crates/tinymold-link-tests/`.
5. **Lint fixes for a newer toolchain** (rustc 1.99's clippy and rustdoc):
   `as_chunks_mut::<4>()` in two PLT writers in `tinymold-link-elf/arch/arm64.rs`;
   an `#[expect(clippy::chunks_exact_to_as_chunks)]` with its reason on
   `FileRecord::write_all` in `tinymold-link-common/record.rs`; bare URLs in
   doc comments wrapped in `<…>`, and two doc comments in
   `tinymold-link-macho/arch.rs` that rustdoc misread as HTML or a private
   link.
6. **`rustfmt.toml`** copied into each crate, so `cargo fmt --all` formats the
   fork with mold's settings and the diff against upstream stays small.

## Workspace rules that do not apply here

The forked crates do not opt into `[workspace.lints]`: mold uses `unsafe`
throughout (with `// SAFETY:` comments inherited from upstream), aborts with
`_exit` on link errors and has no `missing_docs` coverage. They also do not
follow the `src/<feature>/mod.rs` layout or the per-file 90% unit-test
coverage rule. Their regression suite is mold's shell tests, which CI runs as
a separate job. New tinymold code goes into crates that follow every
workspace rule, and the forked crates only call into it.

## Taking upstream changes

```sh
git -C ../mold diff c522789 <new> -- common elf macho arch/elf-x86_64 \
  arch/elf-arm64 arch/macho-arm64 arch/macho-x86_64 cli tests > up.patch
# Rewrite paths: common/ -> crates/tinymold-link-common/, and so on, then
git apply --3way up.patch
```

After a merge, update the commit at the top of this file and `MOLD_VERSION`.
