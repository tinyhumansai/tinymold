# tinymold-link

A linker for Rust edit-build-run loops, forked from
[mold](https://github.com/rui314/mold) 3.0. Today it links exactly as mold
does, for ELF (x86_64, arm64) and Mach-O (arm64, x86_64). The point of the
fork is to add what mold deliberately does not have: state that survives from
one link to the next, so the dependencies that did not change are not read,
parsed and merged again on every link. See the
[spec](../../docs/specs/linker.md), the [plan](../../docs/plans/linker.md) and
[ADR 0009](../../docs/adr/0009-fork-mold-into-a-stateful-linker.md).

## Use

Linux, through the C compiler driver:

```toml
# .cargo/config.toml
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=--ld-path=/path/to/tinymold-link"]
```

macOS: the Mach-O linker runs when the executable is invoked as `ld64` or
`ld64.<anything>`, so link it under that name and point clang at it:

```sh
ln -s /path/to/tinymold-link ~/.local/bin/ld64.tinymold
```

```toml
[target.aarch64-apple-darwin]
rustflags = ["-C", "link-arg=-fuse-ld=/Users/you/.local/bin/ld64.tinymold"]
```

`tinymold-link --version` and `ld64.tinymold -v` print `mold 3.0.0
(tinymold-link …)` and `mold-macho 3.0.0 (tinymold-link …)`.

## Tests

mold's shell suite runs against this binary when asked to, because it takes
minutes and needs compilers and binutils:

```sh
TINYMOLD_LINK_SHELL_TESTS=1 cargo test -p tinymold-link --test integration -- --native
```

On macOS that runs the Mach-O suite, elsewhere the ELF suite.

## License

MIT, as upstream; see [LICENSE](LICENSE) and [FORK.md](FORK.md).
