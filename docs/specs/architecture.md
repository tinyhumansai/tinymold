# tinymold architecture

- **Status:** Draft (target architecture; refined per phase before implementation)
- **Owner:** Steven Enamakel
- **Date:** 2026-10-14

This document describes what tinymold is meant to become and how its parts fit
together. It is the target, not a description of shipped code: the
[roadmap](../plans/roadmap.md) says which parts exist and in what order the
rest arrives, and each phase gets its own spec and plan before implementation.
The research behind every decision here is summarized in
[`../research/README.md`](../research/README.md).

## Problem

Rust builds spend their time and disk in a few predictable places:

- the link, which is 40-50% of a debug edit-run loop and is always redone from
  scratch ([linkers §3](../research/linkers.md#3-what-fraction-of-a-rust-build-is-link-time));
- dependents that rebuild because a crate's `.rmeta` changed for reasons that
  do not affect them ([rustc-pipeline §9](../research/rustc-pipeline.md#9-newer-ideas-and-in-flight-proposals));
- worktrees, checkouts and agents that rebuild identical crates because
  absolute paths and mtimes differ ([rustc-pipeline §5b](../research/rustc-pipeline.md#5b-cacheability-what-rustc-outputs-can-be-reused-and-what-breaks-reuse));
- `target/` directories of several GB each, roughly half of it debuginfo,
  duplicated per worktree ([hashing §4](../research/hashing-and-cache-server.md#4-storage-footprint)).

## Goals and non-goals

Goals:

1. Never run rustc or the linker for an action whose inputs match a previous
   run, on any worktree, for any agent, on any machine that shares the cache.
2. Make no-op and near-no-op builds cost milliseconds of tinymold overhead.
3. Make N worktrees of one project cost roughly 1x dependency artifacts plus N
   x workspace crates on disk, within a configurable global budget.
4. Shorten the links that do have to run.
5. Stay correct: a cache hit must be indistinguishable from a rebuild, and
   every miss must be explainable.

Non-goals (see [Out of scope](#out-of-scope)): a new linker, a rustc fork, a
cargo replacement, a custom remote protocol.

## Components

tinymold is a cargo workspace of small crates. Library crates hold the logic;
the `tinymold` binary is a thin CLI over them.

| Crate | Status | Responsibility |
|---|---|---|
| `tinymold-hash` | Phase 0 | The BLAKE3-256 `Digest` type; hashing byte slices, readers and files; lowercase hex encoding and parsing. The single digest type for everything persisted or shared ([ADR 0002](../adr/0002-blake3-for-content-digests.md)). |
| `tinymold-cas` | Phase 0 | Local content-addressed store. Objects live at `objects/<first 2 hex>/<remaining 62 hex>`. Insert writes a temp file in the store, verifies the digest, then renames it into place, so readers never see a partial object; objects are made read-only. Materialization into a destination path uses a selectable strategy, by default the fallback chain reflink → hardlink → copy. |
| `tinymold` | Phase 0 | The CLI. `tinymold hash <files>` prints digests; `tinymold cas put\|get\|has\|verify` operates the store (default location `TINYMOLD_STORE`, then `XDG_CACHE_HOME/tinymold`, then `HOME/.cache/tinymold`); `tinymold doctor` reports the host, the `rustc` version, the sysroot's `rust-lld`, the store, and which of mold, wild, `ld.lld`, `ld64.lld`, clang and sccache are on `PATH`. |
| `tinymold-key` | Planned (Phase 2) | The rustc ActionKey and ActionResult schema, argument classification, and path normalization ([cache-key spec](cache-key.md)). Pure functions, no I/O beyond reading inputs. |
| `tinymold-wrap` | Planned (Phase 2, 4) | The `RUSTC_WRAPPER` and linker shim. Short-lived, native, starts in a few ms. Talks to the server when one is running and runs the same lookup in-process when not. |
| `tinymold-server` | Planned (Phase 3) | The hot cache daemon: stat cache, FSEvents/inotify dirty set, Unix-socket protocol, single-flight dedup, per-key incremental-dir store, storage tiers and eviction. |
| `tinymold-remote` | Planned (Phase 5) | REAPI v2 client for the LAN/remote tier, with zstd-compressed transfers and `FindMissingBlobs` batching. |
| `tinymold-link` | Planned (Phase 4) | Linker autoselect, link-skip, link concurrency governor, test-binary consolidation, dylib dev mode, prelinked dependency archive. |
| `tinymold-bench` | Planned (Phase 1) | Pinned benchmark harness: filesystem, output reuse, fork mode and thread count are fixed and recorded per run ([linkers §2](../research/linkers.md#2-benchmarks-with-sources)). |

Dependency direction is strictly downward: `hash` ← `cas` ← `key` ← `wrap` /
`server` / `link` / `remote` ← `tinymold`. Nothing below `wrap` knows about
cargo's process model, and `hash` and `cas` know nothing about Rust at all.

### Distribution

Every crate is published to crates.io under the workspace's single version,
starting at 0.0.1 with `tinymold`, `tinymold-hash` and `tinymold-cas`. Later
crates are published as they land. Users install the CLI with:

```sh
cargo install tinymold
```

Publishing runs from the release workflow, never by hand, so the published
crates always match a tagged, validated commit. Pre-1.0, the library crates'
APIs may change in any minor release.

## Data flow of a build

tinymold plugs into cargo through two stable hooks: `RUSTC_WRAPPER` for every
rustc invocation and the `linker` setting for every link. Cargo stays in
charge of scheduling, features and the unit graph.

```text
cargo build / test / check
  │
  │ for each unit: RUSTC_WRAPPER=tinymold-wrap  rustc <args>
  ▼
tinymold-wrap ──────────── Unix socket ───────────► tinymold-server
  │ 1. classify argv: crate type, emit,            │ a. digests of inputs from the stat
  │    check vs build, incremental?                 │    cache (only dirty files rehashed)
  │ 2. send (argv, env, cwd) ─────────────────────► │ b. pre-key → recorded input set
  │                                                 │    → ActionKey (tinymold-key)
  │                                                 │ c. ActionCache lookup:
  │                                                 │    hot/warm → cold → remote
  │ 3a. HIT  ◄──── outputs materialized ─────────── │ d. reflink/hardlink outputs into
  │     replay stderr, exit 0                       │    target/, rewrite ${WS} in diagnostics
  │                                                 │
  │ 3b. MISS ◄──── "run it" (single-flight lease) ─ │ e. identical in-flight key from another
  │     exec rustc + injected --remap-path-prefix   │    worktree? wait for it instead
  │ 4. report outputs + dep-info ─────────────────► │ f. hash outputs, insert into CAS,
  │                                                 │    write ActionResult last
  ▼                                                 ▼
cargo continues                                  CAS + ActionCache on disk

link step: linker = tinymold-wrap (link mode)
  hash object/rlib digests + linker args + linker identity
  → unchanged: reuse the previous output (link-skip)
  → cached:    materialize the binary
  → otherwise: run the selected linker (wild / mold / lld / ld-prime)
               under the link concurrency governor, then cache the output
```

With no server running, `tinymold-wrap` performs steps a-f itself against the
local CAS. It is slower (no stat cache, no single-flight) but produces the same
keys and the same results. Correctness never depends on the daemon
([fast-tools §8.6](../research/fast-tools.md#86-daemon-versus-one-shot)).

## The two-level cache

tinymold follows the Go build cache and REAPI model
([ADR 0003](../adr/0003-two-level-content-addressed-cache.md)):

```text
ActionKey ──(ActionCache)──► ActionResult ──(per output digest)──► CAS blob
 digest of every input       manifest: kind, relative path,        immutable bytes,
 that can affect outputs     digest, size, exec bit, stderr        stored once
```

- **ActionKey** is the BLAKE3-256 digest of a canonical encoding of every
  input to one rustc or linker invocation. Its schema is in
  [cache-key.md](cache-key.md).
- **ActionResult** is a small manifest that lists output digests rather than
  holding bytes. It is written only after all of its blobs are in the CAS, so
  a reader that finds a manifest can always materialize it.
- **CAS blobs** are stored once regardless of how many actions, worktrees or
  projects produced them. The 142 byte-identical rlibs measured across two
  unrelated projects are one blob each
  ([hashing §4.2](../research/hashing-and-cache-server.md#42-dedup-potential-across-projects-and-worktrees-measured)).

Splitting keys from blobs is what makes early cutoff, dedup and a REAPI remote
tier fall out of one design rather than three.

## Early cutoff on rmeta digests

A dependent's ActionKey includes the **content digest of each upstream
`.rmeta`** it reads through `--extern`, not the upstream ActionKey. When an
edit to crate A (a comment, a private function body) produces a byte-identical
`A.rmeta`, every dependent of A computes the same key as before and hits the
cache. Cargo still schedules those dependents, because its fingerprints
changed, but each one costs a lookup and a materialization rather than a
compile ([hashing §1.5](../research/hashing-and-cache-server.md#15-early-cutoff-and-output-hashing)).

Two conditions make this work:

1. rmeta must be reproducible, which requires remapped paths and no embedded
   timestamps. Phase 5 measures which edit kinds actually leave rmeta
   unchanged before the feature is turned on by default.
2. Linking actions use the `.rlib` digest instead, because the linker reads
   code, not metadata.

Early cutoff on the *public interface* of a crate (ignoring spans, the RDR
idea) is a stronger form that needs rmeta decoding; it is a Phase 6 research
bet ([rustc-pipeline §10](../research/rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty), idea 11).

## Path normalization

Absolute paths appear in debuginfo (`DW_AT_comp_dir`), `file!()` strings,
`.rmeta` spans and dep-info. Without normalization, two worktrees of the same
commit share nothing for workspace crates.

tinymold normalizes in two places:

- **In the key.** Every path argument, dep-info entry and path-valued env var
  (`CARGO_MANIFEST_DIR`, `OUT_DIR`) is rewritten relative to one of a fixed set
  of roots: `${WS}` (workspace root), `${CARGO_HOME}`, `${SYSROOT}`,
  `${OUT}` (build dir). A path outside every root makes the action
  uncacheable and is reported by the miss explainer rather than silently
  keyed.
- **In the output.** The wrapper injects `--remap-path-prefix` for each root so
  the produced bytes contain the placeholder rather than the machine path.
  The injected flags are excluded from the key, since they are a function of
  the roots. Diagnostics replayed from cache have `${WS}` re-expanded to the
  requesting worktree's path.

Phase 2 adds a check that scans cached outputs for residual absolute paths and
refuses to share an output that contains one.

## Storage tiers and budget

Rust artifacts are large and compress about 5x
([hashing §4.3](../research/hashing-and-cache-server.md#43-compression-measured)).
tinymold stores them in tiers with a global budget. Defaults come from
[hashing §4.5](../research/hashing-and-cache-server.md#45-size-budget-proposal)
and are configurable.

| Tier | Holds | Default budget | Eviction |
|---|---|---|---|
| Hot | Pages of the current graph's `.rlib`/`.rmeta` for each active worktree, plus the last test binary | ≤ 25% of RAM (8 GB on a 32 GB box); one measured project's hot set is ~2.2 GB | Advisory only: `fcntl(F_RDADVISE)` on macOS, `madvise(MADV_WILLNEED)`/`posix_fadvise` on Linux, driven by active-worktree heartbeats. The kernel page cache is the RAM tier; there is no separate RAM store. |
| Warm | Uncompressed CAS: every artifact of the last K builds per workspace and toolchain | 30 GB or 10% of free disk, whichever is smaller | LRU by last materialization, with a per-project quota and a floor that keeps each active workspace's latest complete graph |
| Cold | zstd-compressed blobs evicted from warm, old toolchains | 20 GB compressed (~100 GB logical) | LRU plus age (14 days) |
| Remote | CI-produced artifacts on a REAPI server | Server-side | Server LRU |

The warm tier stays uncompressed because rustc and the linker mmap `.rlib`
and `.rmeta` files; compressing them would cost a decompression on every
materialization. Promotion from cold or remote to warm happens on a hit.
Eviction only removes blobs no surviving ActionResult in that tier references,
and never removes an object a materialization is in the middle of using.

Outside the CAS, tinymold reduces what gets produced at all through a storage
hygiene profile: deps without debuginfo, `line-tables-only` for workspace
crates, `split-debuginfo = "unpacked"`, `-Zembed-metadata=no` on nightly, and
zstd debug sections on Linux. Together these are expected to cut artifacts
30-50% ([hashing §5](../research/hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold), decision 14).

Target outcome on the measured machine: about 2 GB + 0.3 GB per worktree
instead of 2.5 GB per worktree.

## The hot server

`tinymold-server` is one process per user per machine (see open questions),
reached over a Unix socket. Its protocol carries keys, paths and digests,
never artifact bytes: the server materializes files directly into the
requester's `target/`
([hashing §2.1](../research/hashing-and-cache-server.md#21-keeping-artifacts-hot)).

- **Stat cache.** Maps (dev, inode, size, mtime_ns, ctime_ns) to a digest.
  A file is only rehashed when that tuple changes. Git's racily-clean rule
  applies: a file whose mtime is not older than the entry's write time is not
  trusted.
- **Dirty set.** FSEvents (macOS) and inotify (Linux) watchers per active
  worktree turn "what changed" into an O(changes) query. On a watcher queue
  overflow or restart the server falls back to a full stat walk.
- **Single-flight.** The first request for a key that misses gets a lease to
  run rustc. Concurrent requests for the same key, from any worktree or
  agent, wait for the result instead of compiling it again. A lease expires if
  the holder dies, and a waiter then takes over.
- **Per-key incremental-dir store.** rustc incremental state is local and
  path-sensitive. The server keeps one `incremental/` directory per (crate,
  key without source inputs) and swaps it into place when a worktree or branch
  switch would otherwise start rustc cold
  ([hashing §3](../research/hashing-and-cache-server.md#3-can-a-daemon-keep-rustcs-query-state-warm)).
- **Persistence.** All server state that is expensive to rebuild (stat cache,
  ActionCache index, last-use times) is written to disk so a restart is warm.
  An in-memory-only design is not acceptable.

Expected effect: key computation drops from tens or hundreds of ms to about
1 ms per action, which zccache reports for a similar design [vendor].

## Linking

`tinymold-link` treats the link as another cacheable action and attacks the
links that remain.

- **Autoselect.** Pick the fastest available linker per target (wild or mold
  on Linux, then lld; ld-prime on macOS), configure it through cargo's stable
  linker settings, and verify the choice took effect (for ELF,
  `readelf -p .comment`).
- **Link-skip.** Hash every input object and rlib, the arguments and the
  linker identity. If the hash matches the previous output, reuse it. Cargo
  relinks on mtime churn that changes nothing.
- **Concurrency governor.** `cargo test` launches many parallel links, each
  multi-threaded. A semaphore plus a per-link thread budget bounds peak memory
  ([linkers §1.8](../research/linkers.md#18-other-details)).
- **Fewer links.** Test-binary consolidation merges integration-test binaries
  into one. Dylib dev mode puts dependencies in one shared library so each
  relink touches only the top crate. A prelinked dependency archive reduces
  the final link to a few inputs. All three are opt-in and reversible.
- **Upstream, not a new linker.** Incremental linking is pursued with wild's
  design, not in tinymold
  ([linkers §5](../research/linkers.md#5-incremental-linking-and-hot-patching)).

## Platform notes

Materialization uses one of four strategies, chosen per store with an
override per call:

| Strategy | Mechanism | Shares disk | Shares page cache | Safe for in-place writers |
|---|---|---|---|---|
| Reflink | macOS `clonefile(2)` on APFS; Linux `ioctl(FICLONE)` on btrfs and XFS | Yes (copy-on-write) | No (new inode) | Yes |
| Hardlink | `link(2)` | Yes | Yes | **No** |
| Copy | `copy_file_range(2)` on Linux, `fcopyfile`/`copyfile` on macOS, else read/write | No | No | Yes |
| Auto (default) | Reflink, then hardlink, then copy | | | Guarded, below |

- **macOS.** APFS supports `clonefile`, so Auto almost always reflinks. There
  is no tmpfs; RAM disks compete with the page cache and are not used.
  `split-debuginfo = "unpacked"` is the default and avoids `dsymutil`.
  XProtect scans each new executable on first run (about 1-2 s per
  build-script or test binary), which `tinymold doctor` should detect
  ([rustc-pipeline §6](../research/rustc-pipeline.md#6-linking-link-time-reduction)).
- **Linux.** Reflink works on btrfs and XFS (with `reflink=1`). On ext4 Auto
  falls through to hardlink. Never place the store or `target/` on tmpfs:
  it is charged against RAM and exhausts quickly.
- **Hardlink guard.** CAS objects are read-only (mode 0444), but a hardlinked
  file shares its inode, so a tool that opens it for writing and rewrites it
  in place would corrupt the CAS object. mold and wild deliberately overwrite
  an existing output file in place because it is faster
  ([linkers §1.5](../research/linkers.md#15-io-mmap-vs-read-huge-pages-fallocate-output-reuse)).
  Therefore Auto never hardlinks link outputs (binaries, test executables,
  cdylibs) or anything under `incremental/`; those reflink or copy. Hardlinks
  are reserved for `.rlib`, `.rmeta` and similar inputs that rustc and cargo
  replace by unlink-and-create or rename.
- **Cross-device.** Reflink and hardlink both fail across filesystems; the
  chain falls through to copy; from Phase 1, `tinymold doctor` warns when the store and
  the target dirs are on different devices.

## Correctness strategy

A cache that is wrong once gets switched off. tinymold builds trust with:

1. **Complete keys.** Every input that can affect output is in the key,
   including the rustc binary digest, the sysroot, the linker identity, env
   vars recorded in dep-info, and build-script outputs
   ([cache-key.md](cache-key.md)). An action whose inputs cannot be fully
   determined (a path outside every root, a build script with no
   `rerun-if-*` lines and side effects) is not cached.
2. **Atomic publish.** Blobs go to a temp file, are verified, then renamed;
   the ActionResult is written last. Readers verify each blob's digest the
   first time a server session uses it.
3. **Verification sampling.** A configurable fraction of hits (default 1%) is
   rebuilt in the background and compared byte for byte. Any mismatch is
   reported with both input sets and marks the entry untrusted.
4. **Miss explainer.** `tinymold explain` prints, for any unit, the key inputs
   and the first input that differs from the nearest previous entry (Go's
   `GODEBUG=gocachehash=1` model). Every miss has a stated reason.
5. **Never share unverified incremental outputs.** Outputs produced by a
   rustc that reused an incremental session are stored in the local tier with
   `verified: false` and are not uploaded to the remote tier until
   verification confirms them.
6. **Remote write discipline.** CI writes to the remote tier; developer
   machines read. Per-user namespaces use the REAPI instance name or salt.
7. **Oracles.** CI runs with `-Zincremental-verify-ich` on nightly to catch
   incremental-state bugs that would otherwise be cached.

## Observability

- `tinymold doctor` reports the environment (Phase 0) and, from Phase 1,
  classifies a build as link-, frontend-, LLVM-, macro- or build-script-bound
  from `cargo build --timings` and self-profile data.
- `tinymold explain` (Phase 2) explains misses.
- The server exposes hit rate per tier, bytes per tier, and time saved, the
  health metrics that made Scion's tiers tunable
  ([source-control §2](../research/source-control-and-caching.md#results)).

## Out of scope

- **Writing a linker.** tinymold configures and wraps existing linkers and
  contributes incremental-linking work to wild or mold.
- **Forking rustc or depending on unstable rustc internals by default.**
  Nightly-only features (`-Zchecksum-hash-algorithm`, `-Zthreads`, Cranelift,
  a persistent `rustc_driver` worker) are opt-in accelerators, never
  requirements.
- **Replacing cargo.** Cargo keeps the unit graph, features and scheduling.
- **A custom remote protocol.** The remote tier is REAPI v2.
- **A RAM-disk tier or tmpfs storage.**
- **Virtual filesystems.** A FUSE/NFS-backed `target/` is a possible later
  idea, not part of this architecture.
- **Distributing unverifiable prebuilt binaries.**

## Open questions

Tracked in [`../research/README.md` §6](../research/README.md#6-open-questions-for-planning).
The ones that most affect this document:

- Server scope: per user or per machine, and the trust model if shared.
- Whether incremental compilation stays on for cached workspace crates.
- Whether reflinks' lack of page-cache sharing is measurable enough to prefer
  hardlinks for `.rlib`/`.rmeta` inputs.
