# tinymold

tinymold makes Rust builds fast by never compiling the same thing twice. It
sits in front of rustc as cargo's `RUSTC_WRAPPER`, names every compile by the
content of its inputs, and restores the outputs from a content-addressed
store with a copy-on-write clone. In the spirit of
[mold](https://github.com/rui314/mold): measure where the time goes, then
remove that work rather than doing it slightly faster.

> [!WARNING]
> **🚧 DO NOT USE TINYMOLD FOR PRODUCTION BUILDS.** Use it only for
> development and testing workflows for fast iterative changes.
> tinymold is in its early stages and could have bugs, so it is not suitable
> for building production code.

## Every cargo command, from cache

Developers, CI jobs and coding agents run the same four commands over and
over: `cargo build`, `cargo clippy`, `cargo test` and `cargo llvm-cov`. Each
one builds its own flavor of the dependency graph, and each one starts from
scratch in a fresh CI runner, a new worktree or after a `cargo clean`.
tinymold caches all four.

Below, each command runs in a fresh `target/` directory, once with plain
cargo and once with tinymold's store already warm: what a CI job sees after
restoring its cache, or a second worktree on the same machine.

<!-- commands:start -->
<!-- commands:end -->

`test run` is the tests themselves executing; tinymold serves their build,
not their run (a test result cache is [in progress](docs/specs/test-cache.md)).
Cold runs, sccache, and how each case is measured are in
[`bench/COMMANDS.md`](bench/COMMANDS.md); reproduce them with
`python3 bench/bench.py commands`.

## Install

```sh
cargo install tinymold
tinymold doctor        # host, toolchain, store, fast linkers on PATH
```

Prebuilt binaries for Linux, macOS, and Windows (x86_64 and ARM64) are attached
to each [GitHub release](https://github.com/tinyhumansai/tinymold/releases).

## Use it

**On your machine**, turn it on for every cargo command:

```toml
# ~/.cargo/config.toml, or .cargo/config.toml in a project
[build]
rustc-wrapper = "tinymold"
```

Every worktree, branch and agent sandbox on the machine then shares one
store, so a second checkout of the same commit builds from cache.

**In GitHub Actions**, add two steps around your cargo commands:

```yaml
steps:
  - uses: actions/checkout@v7
  - uses: dtolnay/rust-toolchain@stable
  - uses: tinyhumansai/tinymold@v0          # RUSTC_WRAPPER + restore the store
  - run: cargo clippy --all-targets -- -D warnings
  - run: cargo test --all-features
  - run: cargo llvm-cov --workspace --lcov --output-path lcov.info
  - uses: tinyhumansai/tinymold/save@v0     # gc, hit-rate summary, save
    if: always()
```

The store travels in GitHub's own cache, so no server is needed. Self-hosted
runners keep a persistent store instead (`backend: none`), and many
repositories or runners can share a [bazel-remote](deploy/bazel-remote/README.md)
server (`backend: remote`). Inputs, cache keys and runners are in
[`docs/specs/github-actions.md`](docs/specs/github-actions.md).

**Anywhere else** (GitLab, Buildkite, Jenkins, custom runners), set
`RUSTC_WRAPPER=tinymold`, cache the `TINYMOLD_STORE` directory between jobs,
and run `tinymold gc` before saving it.

**Coding agents:** [`llms.txt`](llms.txt) is a step-by-step integration guide
for all of the above, written for an agent to follow.

## More commands

```sh
# See what the cache did.
TINYMOLD_LOG=/tmp/tm.log cargo build
tinymold stats --log /tmp/tm.log           # hits, misses and time

# Keep the store bounded (the wrapper also collects in the background).
tinymold gc --max-age 14d --max-size 20G

# Start a new worktree warm, and survive branch switches.
tinymold snapshot save                     # in the worktree you built
tinymold snapshot restore                  # in a new worktree, before cargo build
tinymold fresh restore                     # after a checkout: undo mtime-only changes

# The store itself.
tinymold hash src/main.rs                  # BLAKE3 digests, b3sum format
tinymold cas put target/debug/deps/libfoo-*.rlib
```

The store defaults to `$TINYMOLD_STORE`, then `$XDG_CACHE_HOME/tinymold`, then
`~/.cache/tinymold`. Keep it on the same filesystem as your checkouts: that is
what lets a restore be a reflink or hardlink instead of a copy.

For the edit loop itself, set `codegen-units = 64` under `[profile.dev]`. In
interleaved A/B runs it made every edit kind 13-21% faster on ripgrep and bat
than cargo's incremental default of 256
([details](docs/plans/edit-loop.md)).

## Why it is fast

**A hit costs a clone, not a copy.** tinymold stores each artifact once, named
by its BLAKE3 digest, and restores it with a copy-on-write clone where the
filesystem supports one (APFS, btrfs, XFS). Restoring a 512 MiB artifact on an
M-series Mac:

| Method | Median |
| --- | ---: |
| Byte copy | 223.4 ms |
| `tinymold cas get` (reflink) | 3.5 ms |
| `tinymold cas get` (hardlink) | 2.9 ms |

**Keys are content, not paths or times.** Two worktrees at different paths,
or a checkout that touched every mtime, produce the same keys, so they share
every entry ([cache-key spec](docs/specs/cache-key.md)). On ripgrep, a second
worktree of the same commit builds in 0.08s instead of 3.39s, and a branch
switch A→B→A in 0.43s instead of 1.18s
([scenarios](bench/README.md#edit-loop-scenarios-darwin-arm64-18-cpus-rustc-1990-b940084d7-2026-09-28-3-runs-2026-10-14)).

**It caches what other caches skip.** sccache refuses incremental builds and
skips binaries and proc-macros. tinymold caches every rustc call cargo makes,
plus `clippy-driver` with its lints replayed, plus the instrumented compiles
of `cargo llvm-cov`.

**It never trades correctness for speed.** Anything that is an input is in the
key; anything that is not stays out. Objects are immutable once stored, and a
remote server can only make a build miss, never restore different bytes. When
in doubt, tinymold misses.

The cache never links for you: a binary that depends on the crate you edited
is still relinked in full, while unchanged crates, binaries and test
executables are restored without compiling or linking. Every benchmark, the
machine it ran on, and how to reproduce it are in [`bench/`](bench/README.md).

## How it works

```text
cargo ──▶ tinymold (RUSTC_WRAPPER)
              │  key = BLAKE3(normalized rustc invocation, input digests)
              ▼
         key ─▶ ActionResult (output digests) ─▶ CAS objects
              │                                   │
              │  local store first, then an       │  reflink / hardlink
              │  optional REAPI server            │  into target/
```

tinymold attacks four costs: **caching** (done: the unit cache, local and
remote), **hashing** (done: BLAKE3-256 for anything stored,
[ADR 0002](docs/adr/0002-blake3-for-content-digests.md)), a **hot cache
server** that keeps stat caches and hot artifacts in memory (designed), and
**linking** (pick the fastest linker per platform and skip links whose inputs
did not change; designed). The full design is in
[`docs/specs/architecture.md`](docs/specs/architecture.md), the delivery plan
in the [roadmap](docs/plans/roadmap.md), and the reasoning, with sources, in
[`docs/research/`](docs/research/README.md).

## Crates

| Crate | What it is |
| --- | --- |
| [`tinymold`](crates/tinymold) | The `tinymold` binary and its library half |
| [`tinymold-cas`](crates/tinymold-cas) | Local content-addressed store: atomic insert, read-only objects, reflink → hardlink → copy restore |
| [`tinymold-hash`](crates/tinymold-hash) | The BLAKE3-256 `Digest` type, streaming file hashing, and a field hasher for cache keys |
| [`tinymold-key`](crates/tinymold-key) | rustc argument parsing, path roots, dep-info, and the two cache keys |
| [`tinymold-wrap`](crates/tinymold-wrap) | The `RUSTC_WRAPPER`: lookup, restore, run and store, plus the record catalog GC uses |
| [`tinymold-remote`](crates/tinymold-remote) | The remote tier: a hand-written REAPI v2 client for bazel-remote and other caches |

All crates release together under one version and are published to
[crates.io](https://crates.io/crates/tinymold).

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --all-targets --all-features
cargo test --all-features
```

Those four checks are exactly what CI runs. CI also requires at least 90% line
coverage in every source file, builds rustdoc with `-D warnings`, checks the
MSRV, runs `cargo deny`, and packages every crate for crates.io.
[`AGENTS.md`](AGENTS.md) (symlinked as `CLAUDE.md`) is the full guide for
humans and coding agents working on tinymold itself.

## Documentation

- [`llms.txt`](llms.txt): integrating tinymold into a project, for coding agents
- [`docs/specs/`](docs/specs/README.md): behavior specifications, starting
  with the [architecture](docs/specs/architecture.md),
  [rustc wrapper](docs/specs/rustc-wrapper.md) and
  [cache key](docs/specs/cache-key.md)
- [`docs/research/`](docs/research/README.md): what mold, wild, Bun, Zig, Go,
  Bazel, Buck2, Scion, and rustc teach about build speed, with sources
- [`docs/plans/roadmap.md`](docs/plans/roadmap.md): the phased delivery plan
- [`docs/adr/`](docs/adr/): architecture decision records
- [`bench/`](bench/README.md): benchmarks, results, and how to reproduce them

## License

GPL-3.0-only. See [LICENSE](LICENSE).
