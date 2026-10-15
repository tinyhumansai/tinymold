# tinymold

tinymold is a fast content-addressed build cache: one server and one store,
with adapters for each ecosystem's cache protocol. Rust gets the deepest
integration, through a rustc wrapper that caches every compile by content.
Turborepo, Nx, Gradle, ccache, Bazel and Go are served through their own
remote-cache protocols ([ADR 0009](docs/adr/0009-a-cache-server-with-protocol-front-ends.md)).
In the spirit of [mold](https://github.com/rui314/mold): measure where the
time goes, then remove that work rather than doing it slightly faster.


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

For Rust, tinymold attacks the compile, cold-process and disk costs by
**caching** and **hashing**. A link that has to run still runs; linked
binaries whose inputs did not change come back from the cache. The
reasoning, with sources, is in [`docs/research/`](docs/research/README.md).

> **Status: a working unit cache.** The content-addressed store, the hashing
> layer, `tinymold doctor`, `tinymold fresh`, `tinymold snapshot` and the
> rustc wrapper (`RUSTC_WRAPPER=tinymold`) and the test result cache
> (`tinymold run-test`) work today. The cache server and its protocol
> front-ends are specified but not built yet. See the
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

# Replay passing test runs whose binary and inputs did not change.
CARGO_TARGET_<TRIPLE>_RUNNER="tinymold run-test" RUSTC_WRAPPER=tinymold cargo test

# Keep a warm target/ per commit; start a new worktree from the best match.
tinymold snapshot save              # in the worktree you built
tinymold snapshot restore           # in a new worktree, before cargo build
tinymold snapshot restore --replace # after a branch switch
```

For the edit loop itself, set `codegen-units = 64` under `[profile.dev]`. In
interleaved A/B runs it made every edit kind 13-21% faster on ripgrep and bat
than cargo's incremental default of 256, and clean builds moved by only
−6% (ripgrep) and +2% (bat)
([details](docs/plans/edit-loop.md)).

The wrapper is specified in
[`docs/specs/rustc-wrapper.md`](docs/specs/rustc-wrapper.md); set
`TINYMOLD_LOG=<file>` to see a `hit`/`miss` line per compile, or
`TINYMOLD_TRACE=<file>` for a Chrome trace event per wrapper step (see
[`bench/PROFILING.md`](bench/PROFILING.md)). `fresh` and
`snapshot` are specified in
[`docs/specs/freshness.md`](docs/specs/freshness.md), including why a restore
can never make cargo reuse an output built from different content.

As a runner, set once in `.cargo/config.toml`:

```toml
[target.'cfg(all())']
runner = ["tinymold", "run-test"]
```

`tinymold run-test` keys a run by the test binary's digest, its arguments,
the files under its package, and its environment, and replays only passes
([`docs/specs/test-cache.md`](docs/specs/test-cache.md)). A package whose
tests read files outside it declares them under
`[package.metadata.tinymold.test]`, or opts out with `cache = false`.

The store defaults to `$TINYMOLD_STORE`, then `$XDG_CACHE_HOME/tinymold`, then
`~/.cache/tinymold`. Keep it on the same filesystem as your checkouts: that is
what lets a restore be a reflink or hardlink instead of a copy.

```sh
tinymold stats --log "$TINYMOLD_LOG"   # hit rate and time, from the wrapper log
tinymold gc --max-age 14d --max-size 20G   # drop entries unused for two weeks,
                                           # then the least recently used
```

Every hit records when an entry was last used, and `gc` expires by that
([`docs/specs/gc.md`](docs/specs/gc.md)).

## In GitHub Actions

```yaml
steps:
  - uses: actions/checkout@v7
  - uses: dtolnay/rust-toolchain@stable
  - uses: tinyhumansai/tinymold@v0          # RUSTC_WRAPPER + restore the store
  - run: cargo test --all-features
  - run: cargo llvm-cov --workspace --lcov --output-path lcov.info
  - uses: tinyhumansai/tinymold/save@v0     # gc, hit-rate summary, save
    if: always()
```

By default the store travels in GitHub's own cache, so no server is needed.
Cargo's crate downloads get a cache entry of their own, keyed by
`Cargo.lock`.
For many repositories or runners, run a shared
[bazel-remote](deploy/bazel-remote/README.md) and use the action's `remote`
backend (`remote-url`, plus `remote-auth` from a secret). Every local miss
then asks the server first, and the job pushes what it built at the end.
Inputs, cache keys and self-hosted runners are covered in
[`docs/specs/github-actions.md`](docs/specs/github-actions.md), and the remote
tier in [`docs/specs/remote.md`](docs/specs/remote.md).

The cache never links for you. A binary that depends on the crate you edited
is still relinked in full, while unchanged crates, binaries and test
executables are restored without compiling or linking.

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
 Turbo   Nx   Gradle   ccache   Bazel/Buck2   rustc wrapper   go (GOCACHEPROG)
   └──────┴──────┴────────┴──────────┴──────────────┴──────────────┘
                                  │ each speaks its own protocol
                                  ▼
          tinymold serve: front-ends (Turbo, Nx, Gradle, Bazel HTTP, REAPI, native)
                                  │
          tinymold-store: namespaces, ActionCache, BLAKE3 ↔ SHA-256 index, LRU budget
                                  │
          tinymold-cas: immutable objects by BLAKE3, reflink/hardlink restore
```

- **One store for every language.** Each toolchain keeps computing its own
  keys; the server stores and serves the results, and identical bytes are
  stored once whichever protocol they arrived through.
- **Runs anywhere.** On a developer machine, as a shared service, or inside a
  GitHub Actions job on `localhost` with the store carried in `actions/cache`.
- **Rust goes deeper.** The rustc wrapper caches every unit by content, with
  path normalization so worktrees at different paths share artifacts
  ([cache-key spec](docs/specs/cache-key.md)), and replays passing test runs.
- **Hashing:** BLAKE3-256 for anything stored or shared
  ([ADR 0002](docs/adr/0002-blake3-for-content-digests.md)), and a stat cache so
  unchanged files are never read at all.

The server design is in [`docs/specs/cache-server.md`](docs/specs/cache-server.md),
the Rust side in [`docs/specs/architecture.md`](docs/specs/architecture.md).

## Crates

| Crate | What it is |
| --- | --- |
| [`tinymold`](crates/tinymold) | The `tinymold` binary and its library half |
| [`tinymold-cas`](crates/tinymold-cas) | Local content-addressed store: atomic insert, read-only objects, reflink → hardlink → copy restore |
| [`tinymold-store`](crates/tinymold-store) | The cache server's core: namespaced entries over the CAS, and a SHA-256 index so every protocol finds the same blob |
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
