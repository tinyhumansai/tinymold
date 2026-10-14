# tinymold

tinymold makes Rust builds fast and small. It is a build accelerator for cargo
in the spirit of [mold](https://github.com/rui314/mold): measure where the time
goes, then remove that work rather than doing it slightly faster.


> [!WARNING]
> **🚧 DO NOT USE TINYMOLD FOR PRODUCTION BUILDS.** Use it only for
> development and testing workflows for fast iterative changes.
> tinymold is in it's early stages and could have bugs hence not suitable for
> building production level code.

A Rust edit-build-test loop pays the same costs over and over:

- **Compiling what was already compiled somewhere.** Every worktree, branch
  switch, CI job, and AI agent rebuilds the same dependency graph. Cargo's own
  cross-workspace cache has not shipped, and sccache skips binaries,
  proc-macros, and incremental builds.
- **Relinking from scratch.** About half of an incremental debug rebuild is the
  linker, and no mainstream linker is incremental.
- **Cold processes.** Every `cargo` invocation rediscovers state by walking the
  filesystem and rehashing unchanged files.
- **Gigabytes of `target/`.** Every worktree keeps its own copy of every
  dependency artifact, and every test binary carries its own copy of every
  dependency's debug info. A medium project reaches 4–5 GB per checkout.

tinymold attacks these four costs: **caching**, **linking**, a **hot cache
server**, and **hashing**, with **disk footprint** as a first-class goal. The
reasoning, with sources, is in [`docs/research/`](docs/research/README.md).

> **Status: a working unit cache.** The content-addressed store, the hashing
> layer, `tinymold doctor`, `tinymold fresh`, `tinymold snapshot` and the
> rustc wrapper (`RUSTC_WRAPPER=tinymold`) work today. The cache server and
> linker work are designed but not built yet. See the
> [roadmap](docs/plans/roadmap.md).

## Install

```sh
cargo install tinymold
```

Prebuilt binaries for Linux, macOS, and Windows (x86_64 and ARM64) are attached
to each [GitHub release](https://github.com/tinyhumansai/tinymold/releases).

## Quick start

```sh
tinymold doctor                     # host, toolchain, fast linkers on PATH
tinymold hash src/main.rs           # BLAKE3 digests, b3sum format
tinymold cas put target/debug/deps/libfoo-*.rlib
tinymold cas get <digest> /other/worktree/target/debug/deps/libfoo.rlib

# After a build: remember what the target was built from.
tinymold fresh record
# After a checkout, rebase or touch: undo mtime changes that changed nothing.
tinymold fresh restore

# Cache every rustc invocation by content, across worktrees.
RUSTC_WRAPPER=tinymold cargo build

# Keep a warm target/ per commit; start a new worktree from the best match.
tinymold snapshot save              # in the worktree you built
tinymold snapshot restore           # in a new worktree, before cargo build
tinymold snapshot restore --replace # after a branch switch
```

The wrapper is specified in
[`docs/specs/rustc-wrapper.md`](docs/specs/rustc-wrapper.md); set
`TINYMOLD_LOG=<file>` to see a `hit`/`miss` line per compile. `fresh` and
`snapshot` are specified in
[`docs/specs/freshness.md`](docs/specs/freshness.md), including why a restore
can never make cargo reuse an output built from different content.

The store defaults to `$TINYMOLD_STORE`, then `$XDG_CACHE_HOME/tinymold`, then
`~/.cache/tinymold`. Keep it on the same filesystem as your checkouts: that is
what lets a restore be a reflink or hardlink instead of a copy.

## Why it can be fast

One object, many worktrees. tinymold stores each artifact once, named by its
BLAKE3 digest, and restores it with a copy-on-write clone where the filesystem
supports one (APFS, btrfs, XFS). Restoring a 512 MiB artifact on an M-series
Mac:

| Method | Median |
| --- | ---: |
| Byte copy | 223.4 ms |
| `tinymold cas get` (reflink) | 3.5 ms |
| `tinymold cas get` (hardlink) | 2.9 ms |

That is the cost of a cache hit, and why a hit beats a rebuild even for very
large artifacts. Full tables, charts, the ripgrep edit-loop baseline per linker,
and how to reproduce them are in [`bench/`](bench/README.md).

## How it will work

```text
cargo ──▶ tinymold wrap (RUSTC_WRAPPER + linker shim)
              │  ActionKey = BLAKE3(normalized rustc invocation, input digests)
              ▼
         tinymold server (per-machine daemon, hot)
              │  stat cache + FS events: no rehashing unchanged files
              │  single-flight: N worktrees asking for one artifact build it once
              ▼
         ActionKey ─▶ ActionResult (output digests) ─▶ CAS blobs
              │                                         │
              │  early cutoff: dependents key on the    │  reflink/hardlink into
              │  dependency's .rmeta digest, so an edit │  target/, zstd for cold
              │  that leaves the interface unchanged    │  tiers, LAN/remote tier
              │  rebuilds nothing downstream            │  over the Remote Execution API
```

- **Caching:** a two-level, content-addressed cache in the style of Go's build
  cache and Bazel's Remote Execution API, with path normalization so worktrees
  at different paths share artifacts
  ([cache-key spec](docs/specs/cache-key.md)).
- **Linking:** pick the fastest linker per platform, skip links whose inputs
  didn't change, link tests once instead of once per file, and build
  dependencies as one shared dev dylib. Longer term, contribute incremental
  linking to [wild](https://github.com/davidlattimore/wild) rather than write a
  new linker.
- **Hot cache server:** a per-machine daemon that keeps the stat cache, file
  watcher, and hot artifacts in memory, with plain one-shot mode as the
  correctness fallback.
- **Hashing:** BLAKE3-256 for anything stored or shared
  ([ADR 0002](docs/adr/0002-blake3-for-content-digests.md)), and a stat cache so
  unchanged files are never read at all.

The full design is in [`docs/specs/architecture.md`](docs/specs/architecture.md).

## Crates

| Crate | What it is |
| --- | --- |
| [`tinymold`](crates/tinymold) | The `tinymold` binary and its library half |
| [`tinymold-cas`](crates/tinymold-cas) | Local content-addressed store: atomic insert, read-only objects, reflink → hardlink → copy restore |
| [`tinymold-hash`](crates/tinymold-hash) | The BLAKE3-256 `Digest` type, streaming file hashing, and a field hasher for cache keys |

All crates release together under one version and are published to
[crates.io](https://crates.io/crates/tinymold).

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --all-targets --all-features
cargo test --all-features
cargo run -p tinymold --example basic
```

Those four checks are exactly what CI runs. CI also requires at least 90% line
coverage in every source file, builds rustdoc with `-D warnings`, checks the
MSRV, runs `cargo deny`, and packages every crate for crates.io.
[`AGENTS.md`](AGENTS.md) (symlinked as `CLAUDE.md`) is the full guide for
humans and coding agents.

## Documentation

- [`docs/research/`](docs/research/README.md): what mold, wild, Bun, Zig, Go,
  Bazel, Buck2, Scion, and rustc teach about build speed, with sources
- [`docs/specs/architecture.md`](docs/specs/architecture.md): the target design
- [`docs/specs/cache-key.md`](docs/specs/cache-key.md): what goes into a rustc
  cache key, and what breaks reuse
- [`docs/plans/roadmap.md`](docs/plans/roadmap.md): the phased delivery plan
- [`docs/adr/`](docs/adr/): architecture decision records
- [`bench/`](bench/README.md): benchmarks, results, and how to reproduce them

## License

GPL-3.0-only. See [LICENSE](LICENSE).
