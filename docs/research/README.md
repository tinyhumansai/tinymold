# Research: where Rust build time and disk go, and what tinymold does about it

This directory holds the research that shaped tinymold's architecture. The five
reports are kept as written (dated 2026-10-14) so their sources and confidence
labels stay intact. This README is the synthesis: the numbers that define the
problem, the principles every design decision follows, a de-duplicated catalogue
of ideas grouped by pillar, what tinymold will not do, and what is still open.

| Report | Covers |
|---|---|
| [`linkers.md`](linkers.md) | mold, wild, lld, Apple ld-prime; link share of Rust builds; incremental linking; debuginfo and `target/` size |
| [`rustc-pipeline.md`](rustc-pipeline.md) | rustc and cargo stages, incremental compilation, parallelism, backends, proc macros, what breaks cache reuse |
| [`hashing-and-cache-server.md`](hashing-and-cache-server.md) | Hash choice, avoiding hashing, daemons and remote caches, measured `target/` footprint, a draft cache-key schema |
| [`source-control-and-caching.md`](source-control-and-caching.md) | Scion/OpenScion, Piper/CitC/Bazel, EdenFS/Buck2, REAPI, Nix CA derivations; content addressing at scale |
| [`fast-tools.md`](fast-tools.md) | Bun, Zig, esbuild, Go's build cache, tsgo; transferable engineering principles |

Confidence labels are carried over from the reports: **[measured]** (on the
author's machine), **[vendor]** (a project's own benchmark), **(unverified)** or
**[memory]** (not confirmed from a primary source). Anything so marked must be
re-checked before tinymold depends on it.

The resulting design is in [`../specs/architecture.md`](../specs/architecture.md),
the cache key in [`../specs/cache-key.md`](../specs/cache-key.md), and the
delivery order in [`../plans/roadmap.md`](../plans/roadmap.md).

## 1. The problem in numbers

### Where the time goes

- **Incremental rebuilds after small edits are the main pain.** In the 2025
  Rust compiler performance survey (3,700+ responses), 55% wait more than 10 s
  to rebuild their worst project, and 42% had tried no build-performance
  setting at all ([rustc-pipeline §0](rustc-pipeline.md#0-executive-takeaways)).
- **Linking is 40-50% of the debug edit-run loop.** ripgrep spends about half
  its debug build in the linker; Lattimore measured 454 ms of 939 ms (48%)
  ([linkers §3](linkers.md#3-what-fraction-of-a-rust-build-is-link-time)).
  The link is always from scratch: no production Rust linker is incremental yet
  ([linkers §5](linkers.md#5-incremental-linking-and-hot-patching)).
- **The linker choice alone swings a link by 10x or more.** A warm `cargo run`
  went from 20.2 s with GNU ld to 1.60 s with lld and 1.16 s with mold plus
  static musl ([linkers §2](linkers.md#2-benchmarks-with-sources)). rust-lld is
  the default only on `x86_64-unknown-linux-gnu` since 1.90; other targets and
  distro toolchains may still use GNU ld.
- **`cargo test` multiplies links.** Each `tests/*.rs` file, unit-test target,
  example and doctest is its own binary that statically links every dependency.
  Merging integration tests into one crate was reported about 50% faster
  ([linkers §2](linkers.md#2-benchmarks-with-sources)).
- **Dependents rebuild needlessly.** Almost any edit changes a crate's `.rmeta`
  (spans, source map), and cargo propagates that to every dependent. Upstream
  "Relink, Don't Rebuild" claims 5-10x on such edits but is marked "will not
  complete" ([rustc-pipeline §9](rustc-pipeline.md#9-newer-ideas-and-in-flight-proposals)).
- **The existing cache misses the crates people edit.** sccache cannot cache
  incremental compiles or `bin`/`dylib`/`cdylib`/`proc-macro` crates, and
  absolute paths break cross-checkout hits
  ([rustc-pipeline §5](rustc-pipeline.md#5-caching)). Workspace crates in two
  worktrees are two different units to cargo
  ([source-control §3.6](source-control-and-caching.md#36-cargorustc-today-where-the-gaps-are)).
- **Warm state wins by orders of magnitude.** Zig re-analyzes a 500k-line
  codebase in ~63 ms warm versus ~14 s cold; Bun resolves cached manifests in
  9.1 ms; zccache reports ~1 ms warm hits versus ~170 ms for sccache [vendor]
  ([fast-tools §3.3](fast-tools.md#33-incremental-compilation-and-in-place-binary-patching),
  [hashing §2](hashing-and-cache-server.md#2-persistent-daemons-and-remote-caches)).
- **Hashing itself is not the bottleneck.** A whole 4.6 GB `target/` hashes in
  about 2.4 s with single-thread SHA-256 [measured]; BLAKE3 is several times
  faster. The cost is in *reading* files, so the win is not reading them
  ([hashing §1.1](hashing-and-cache-server.md#11-throughput-landscape)).

### Where the bytes go

- **A real dev `target/` is several GB.** Measured: 4.6 GB, 2.4 GB and 1.5 GB
  for three projects. In the 4.6 GB one, `deps/` held 1,596 MB of `.rlib`,
  633 MB of `.rmeta` that is duplicated inside each rlib, 666 MB in three
  test/bin executables and 545 MB of leftover `.o` files [measured]
  ([hashing §4.1](hashing-and-cache-server.md#41-where-the-bytes-go-measured-this-machine)).
- **Debuginfo is ~50% of the bytes and incremental state ~35%.** Cargo's own
  dev target dir is 3,151 MiB with both on and 1,000 MiB with both off;
  `-Zembed-metadata=no` saves another ~240 MiB
  ([hashing §4.1](hashing-and-cache-server.md#41-where-the-bytes-go-measured-this-machine)).
- **Linux copies dependency DWARF into every binary.** Cargo's own tests take
  about 7 GB on Linux versus 2.2 GB on macOS for that reason
  ([linkers §4](linkers.md#4-disk-footprint-how-linking-inflates-target)).
- **Most of it is shareable.** Across two unrelated projects, 142 of 144 rlibs
  with matching `name-<hash>` filenames were byte-identical. Across two
  worktrees of one project, 168 of 209 matched, but **41 differed despite
  identical filenames**, so a filename is not a content key [measured]
  ([hashing §4.2](hashing-and-cache-server.md#42-dedup-potential-across-projects-and-worktrees-measured)).
- **Artifacts compress about 5x.** A 665 MB rlib compressed to 126 MB with
  `zstd -3 -T0` in 0.14 s [measured]
  ([hashing §4.3](hashing-and-cache-server.md#43-compression-measured)).
- **Target outcome:** N worktrees should cost about 1x registry deps plus N x
  workspace crates, roughly 2 GB + 0.3 GB x N on the measured machine instead
  of 2.5 GB x N
  ([hashing §4.5](hashing-and-cache-server.md#45-size-budget-proposal)).

## 2. Cross-cutting principles

Every tinymold component is held to these. Each is backed by more than one
report.

1. **Do less work, rather than the same work faster.** Zig skips LLVM for
   debug builds, Go reads only export data, mold's ablation study finds the win
   comes from parallelizing every pass rather than one trick. For tinymold the
   biggest lever is not running rustc or the linker at all
   ([fast-tools §1](fast-tools.md#1-executive-summary),
   [linkers §0](linkers.md#0-state-of-the-world-oct-2026)).
2. **Content-addressed keys.** Key every action by a digest of all its inputs
   and store every output by the digest of its bytes. Never key on mtime or on
   cargo's filename hash
   ([fast-tools §5.2](fast-tools.md#52-build-cache-design-the-model-to-copy),
   [source-control §3.4](source-control-and-caching.md#34-remote-execution-api-rev2-buildbuddy-nativelink-others)).
3. **Early cutoff.** A dependent's key folds in the *content digest* of its
   upstream outputs (the `.rmeta`), not the upstream key. An edit that leaves
   the output bytes unchanged stops propagating (Go content IDs, Nix CA
   derivations, Buck2 DICE)
   ([hashing §1.5](hashing-and-cache-server.md#15-early-cutoff-and-output-hashing),
   [source-control §3.5](source-control-and-caching.md#35-nix-content-addressed-derivations)).
4. **Path normalization.** Absolute paths leak into debuginfo, `file!()`,
   `.rmeta` spans and dep-info. Remap them so N worktrees, agents and machines
   share one key space
   ([rustc-pipeline §5b](rustc-pipeline.md#5b-cacheability-what-rustc-outputs-can-be-reused-and-what-breaks-reuse)).
5. **Materialize by reflink or hardlink, not copy.** Bun measured 2.3-3x over
   plain copy; reflinks cost almost no extra disk on APFS, btrfs and XFS.
   Hardlinks share the page cache but are unsafe for files a tool rewrites in
   place ([fast-tools §8.5](fast-tools.md#85-filesystem-tricks),
   [hashing §2.1](hashing-and-cache-server.md#21-keeping-artifacts-hot)).
6. **A daemon with a one-shot fallback.** Warm state (stat cache, dirty set,
   graph) makes no-op work near-instant, but correctness must never depend on
   the daemon, and its state must serialize to disk from day one (Zig's
   incrementality is still stuck behind `--watch` for lack of that)
   ([fast-tools §8.6](fast-tools.md#86-daemon-versus-one-shot)).
7. **Measure before optimizing.** Filesystem, output reuse, fork and thread
   count change linker results by up to 1.7x, so benchmarks pin all four. Ship
   a miss explainer and a verify mode before asking anyone to trust the cache
   ([linkers §2](linkers.md#2-benchmarks-with-sources),
   [fast-tools §8.2](fast-tools.md#82-how-to-key-the-cache)).

## 3. Idea catalogue

Merged from the five reports' idea lists; duplicates are folded into one row
with every source linked. **Impact:** H = 2x or better on the targeted
workflow, M = 15-50%, L = under 15% or indirect. **Difficulty:** S = days,
M = weeks, L = months, XL = upstream rustc/linker work or a quarter or more.
"Phase" refers to [`../plans/roadmap.md`](../plans/roadmap.md).

Short report names: **L** = linkers, **R** = rustc-pipeline, **H** = hashing
and cache server, **SC** = source control and caching, **F** = fast tools. "L7#2"
means idea 2 in section 7 of the linkers report.

### 3.1 Build caching

| Idea | Impact | Diff. | Phase | Sources |
|---|---|---|---|---|
| Two-level cache: ActionKey → ActionResult manifest → CAS blobs | H (foundation) | M | 2 | [H5#1](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold), [F5.2](fast-tools.md#52-build-cache-design-the-model-to-copy) |
| Path-normalizing rustc wrapper: location-independent keys for workspace crates | H for worktrees and fresh checkouts | M | 2 | [R10#1](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty), [SC5#1](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents), [H5#5](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold), [F9#7](fast-tools.md#9-concrete-ideas-for-tinymold) |
| Cache what sccache cannot: bin, test, cdylib, proc-macro and link outputs | M-H | M | 2 | [H5#7](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold), [F9#4](fast-tools.md#9-concrete-ideas-for-tinymold), [R10#3](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty) |
| Build-script output cache (`OUT_DIR`, `cargo:` lines) keyed by declared `rerun-if-*` inputs | M-H on clean builds | M | 2 | [F9#5](fast-tools.md#9-concrete-ideas-for-tinymold), [SC5#14](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents) |
| Checksum freshness on stable: restore mtimes from a content manifest after checkout or switch | M-H after branch switches | S-M | 2 | [R10#6](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty), [F9#1](fast-tools.md#9-concrete-ideas-for-tinymold) |
| Miss explainer and verify mode (`gocachehash` / `gocacheverify` style) | Enabler | S | 1-2 | [F9#14](fast-tools.md#9-concrete-ideas-for-tinymold), [R10#4](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty), [SC5#15](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents) |
| Early cutoff on rmeta output digests | H on deep graphs | M-L | 5 | [H5#4](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold), [SC5#4](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents), [F9#2](fast-tools.md#9-concrete-ideas-for-tinymold) |
| Commit-addressed pre-warm: CI publishes cache entries for every `main` commit | H for cold starts | S-M | 5 | [SC5#7](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents) |
| Flag and feature stabilizer: move global `RUSTFLAGS`, workspace-hack, separate rust-analyzer target dir | M-H (stops thrash) | S-M | 1 | [R10#5](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty), [H5#12](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold) |
| Test result cache keyed by test binary digest, args and traced file/env reads | H on large suites | M | later | [F9#12](fast-tools.md#9-concrete-ideas-for-tinymold), [SC5#10](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents) |
| Affected-crate selection from a diff against the merge base | H in CI and agent loops | S-M | later | [SC5#10](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents) |
| Serve `cargo check` diagnostics from cache; materialize binaries only when run | M-H for agents | M | later | [SC5#9](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents) |
| Proc-macro expansion cache with a purity allowlist | M (10-35% incremental, estimate) | L | 6 | [R10#12](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty), [F9#13](fast-tools.md#9-concrete-ideas-for-tinymold) |
| RDR-lite: span-insensitive public-interface hash of `.rmeta` | H (5-10x claimed upstream) | XL | 6 | [R10#11](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty), [F9#3](fast-tools.md#9-concrete-ideas-for-tinymold) |
| Verified, reproducibly built proc-macro and `-sys` artifacts (avoids the serde_derive mistake) | H on clean builds | M | later | [R10#13](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty) |
| Nightly turbo mode: `-Zthreads`, Cranelift for workspace crates, `-Zhint-mostly-unused` | M-H | M | later | [R10#17](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty), [F9#9](fast-tools.md#9-concrete-ideas-for-tinymold) |

### 3.2 Linking

| Idea | Impact | Diff. | Phase | Sources |
|---|---|---|---|---|
| Linker autoselect (wild, mold, lld, ld-prime) plus a debuginfo profile (`line-tables-only`, deps `debug=false`, `split-debuginfo=unpacked`) | H vs GNU ld (1.5-15x), M vs lld | S | 1, 4 | [L7#1](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty), [R10#7](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty), [F9#10](fast-tools.md#9-concrete-ideas-for-tinymold) |
| Link-skip shim: hash link inputs and args, skip the link when unchanged | M (removes spurious relinks) | S-M | 4 | [L7#2](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty) |
| Link concurrency governor for `cargo test` (job semaphore, per-link thread budget) | M (avoids OOM and thrash) | S | 4 | [L7#10](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty) |
| Test-binary consolidation: one link for all integration tests | M-H (~50% faster test builds) | M | 4 | [L7#3](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty), [R10#9](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty) |
| Dylib dev mode: dependencies as one shared `dylib` (generalized Bevy `dynamic_linking`) | H for link-bound loops | M-L | 4 | [L7#4](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty), [R10#8](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty) |
| Prelinked dependency archive (`ld -r` or dylib) stored in the cache | M-H | M-L | 4 | [R10#10](rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty) |
| Output reuse, fork-and-detach and fallocate tuning | M (1.2-1.7x link wall time) | S | 4 | [L7#11](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty) |
| DWARF-free main links: unpacked split debuginfo, `.dwp` only when a debugger attaches | M | S-M | 4 | [L7#8](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty) |
| ICF and `--gc-sections` only in a size profile, never the dev loop | L (clarity) | S | 4 | [L7#13](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty) |
| Incremental output patching, contributed to wild's design | H (~10 ms link of one changed CGU) | XL | 6 | [L7#6, #14](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty), [F9#11](fast-tools.md#9-concrete-ideas-for-tinymold) |
| Linker with warm resident state (parsed rlibs, merged strings) | H | L | later | [L7#5](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty) |
| Jump-table hot patching (Subsecond-style) | H, with layout-change limits | L | later | [L7#7](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty) |

### 3.3 Hot cache server

| Idea | Impact | Diff. | Phase | Sources |
|---|---|---|---|---|
| Node-local daemon on a Unix socket; payloads are keys and paths, never artifact bytes | H for no-op and repeat builds | M | 3 | [H5#3](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold), [SC5#2](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents), [F9#8](fast-tools.md#9-concrete-ideas-for-tinymold) |
| Stat cache plus FSEvents/inotify dirty set (O(changes), full walk on overflow) | M-H | M | 3 | [SC5#5](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents), [H1.4](hashing-and-cache-server.md#14-avoiding-hashing-entirely) |
| Single-flight: identical concurrent actions across worktrees and agents run once | H in agent bursts | S-M | 3 | [SC5#11](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents) |
| Per-key incremental-dir store, swapped in on branch or worktree switch | M-H | M | 3 | [H5#8](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold), [H3](hashing-and-cache-server.md#3-can-a-daemon-keep-rustcs-query-state-warm) |
| REAPI v2 client for a LAN/remote tier (zstd, `FindMissingBlobs`) | H for CI and fresh boxes | M | 5 | [H5#11](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold), [SC5#3](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents) |
| Persistent cargo unit-graph cache (skip resolution per command) | M | L | later | [SC5#12](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents), [H2](hashing-and-cache-server.md#2-persistent-daemons-and-remote-caches) |
| Prespawned or persistent `rustc_driver` worker (process-level warm state only) | L-M (tens of ms per invocation) | L | 6 | [H5#15](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold) |
| Lazy, FUSE/NFS-backed `target/` that materializes from the CAS on read | H for short-lived sandboxes | L | later | [SC5#8](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents) |

### 3.4 Hashing

| Idea | Impact | Diff. | Phase | Sources |
|---|---|---|---|---|
| BLAKE3-256 for every persisted or shared digest; XXH3 only behind the stat cache; identity or foldhash for in-memory maps | Safe sharing; hashing stays off profiles | S | 0 | [H1.2](hashing-and-cache-server.md#12-which-hash-where), [H5#2](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold) |
| Stat cache keyed by (dev, inode, size, mtime_ns, ctime_ns) with git's racily-clean rule | M-H | S-M | 3 | [H1.4](hashing-and-cache-server.md#14-avoiding-hashing-entirely), [SC1](source-control-and-caching.md#working-copy-the-part-most-relevant-to-builds) |
| Reuse rustc's own checksums (`-Zchecksum-hash-algorithm=blake3`) where nightly is in use | L-M | S | 2 | [H5#9](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold) |
| Take hashes from git for clean tracked files, mapped once to BLAKE3 and memoized | M-H on key computation | S-M | later | [SC5#6](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents), [H1.4](hashing-and-cache-server.md#14-avoiding-hashing-entirely) |
| Parallel BLAKE3 only for large artifacts; a size floor below which caching is not worth it | L | S | 2 | [H1.1](hashing-and-cache-server.md#11-throughput-landscape), [F8.3](fast-tools.md#83-avoid-redundant-work) |
| Content-defined chunking (FastCDC) for the cold and remote tiers | Unmeasured | M | later | [H1.6](hashing-and-cache-server.md#16-content-defined-chunking) |

### 3.5 Storage footprint

| Idea | Impact | Diff. | Phase | Sources |
|---|---|---|---|---|
| Read-only CAS with reflink → hardlink → copy materialization | 2-3x faster restores; near-zero extra disk | S | 0 | [H5#6](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold), [F9#6](fast-tools.md#9-concrete-ideas-for-tinymold) |
| Tiered storage with quotas: hot page cache, warm CAS, cold zstd, remote | Up to ~4-5x fewer bytes for N worktrees | M | 3 | [H4.5](hashing-and-cache-server.md#45-size-budget-proposal), [H5#13](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold) |
| Storage hygiene profile: `-Zembed-metadata=no`, unpacked split debuginfo, deps without debuginfo, zstd debug sections on Linux | 30-50% smaller artifacts | S | 1, 4 | [H5#14](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold), [L4](linkers.md#4-disk-footprint-how-linking-inflates-target) |
| Content-addressed dedup of identical `.dwo` and DWARF across test binaries and worktrees | M-H on disk | M | later | [L7#9](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty) |
| Feature-set canonicalization report (only 144 of 323 shared crate names matched) | M on cross-project hits | S | 1 | [H5#12](hashing-and-cache-server.md#5-fifteen-design-decisions-for-tinymold) |
| Last-use tracking and GC of stale toolchains, profiles and incremental sessions | M on disk | M | 3 | [H4.4](hashing-and-cache-server.md#44-garbage) |
| New agent worktrees start by reflink-cloning a warm `target/` | H for agent sandboxes | S | 3 | [SC5#8](source-control-and-caching.md#5-ideas-for-tinymold-rustcargo-many-worktrees-many-agents) |

## 4. What tinymold will not do

- **Write a new linker from scratch.** mold is already near `cp` speed for full
  links, and wild has incremental linking as its stated goal and a place on
  the Rust 2026 Fast Builds roadmap. tinymold selects and configures existing
  linkers, avoids links it can prove redundant, and contributes incremental
  linking work upstream to wild or mold
  ([linkers §7](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty), idea 14).
- **Fork rustc.** tinymold works through `RUSTC_WRAPPER`, stable flags, and
  opt-in nightly flags. Anything that needs compiler changes (RDR, shareable
  incremental state, warm `TyCtxt`) is pursued upstream or not at all
  ([hashing §3](hashing-and-cache-server.md#3-can-a-daemon-keep-rustcs-query-state-warm)).
- **Replace cargo.** Bazel `rules_rust` and Buck2 + reindeer already exist for
  teams willing to leave cargo; their adoption cost is the point tinymold
  avoids. Users keep `cargo build`, `cargo test` and their `Cargo.toml`
  ([rustc-pipeline §5](rustc-pipeline.md#5-caching)).
- **Invent a remote protocol.** The LAN and remote tier speaks REAPI v2 so
  bazel-remote, NativeLink and BuildBuddy work unmodified.
- **Keep a RAM-disk tier.** The kernel page cache already is the RAM tier, and
  tmpfs counts against RAM on Linux
  ([hashing §2.1](hashing-and-cache-server.md#21-keeping-artifacts-hot)).
- **Ship unverifiable prebuilt binaries.** Precompiled proc macros and `-sys`
  artifacts are only served if they are reproducibly built and auditable
  ([rustc-pipeline §4](rustc-pipeline.md#4-proc-macros-and-build-scripts)).

## 5. Disagreements between the reports

- **Hardlink or reflink first.** The hashing report recommends hardlinks for
  immutable inputs (shared page cache, O(1)) and reflinks for anything a tool
  might touch; the fast-tools report prefers reflinks and allows hardlinks only
  for files that are read-only by contract. tinymold resolves this with a
  selectable strategy whose default chain is reflink → hardlink → copy, plus a
  guard that never hardlinks outputs a known tool rewrites in place
  ([architecture](../specs/architecture.md#platform-notes)).
- **Compressing debug sections.** The linkers report measures zstd debug
  compression at ~0.1 s for a large reduction; the rustc-pipeline report
  suggests `--compress-debug-sections=none` to save CPU (unverified). Phase 1
  benchmarks settle it.
- **`share-generics` default.** The linkers report read rustc source and found
  it on by default at opt-level 0/1/s/z; the rustc-pipeline report marks the
  default unverified. Treat the source reading as current until re-checked.

## 6. Open questions for planning

1. **Incremental versus caching for workspace crates.** Incremental outputs
   are not reproducible. Do we turn incremental off for cached units, keep it
   on and cache only locally as unverified, or rely on the per-key
   incremental-dir store? What is the measured cost of each on the corpus?
2. **Is `.rmeta` deterministic enough for early cutoff on stable?** Which
   edits (comments, private bodies) actually leave rmeta bytes unchanged once
   paths are remapped?
3. **Daemon scope.** Per user or per machine? A per-machine store shared
   across users needs a trust model; per user wastes disk on shared boxes.
4. **What explains the 41 same-name, different-bytes rlibs** across two
   worktrees ([hashing §4.2](hashing-and-cache-server.md#42-dedup-potential-across-projects-and-worktrees-measured))?
   The answer decides which inputs the key must add.
5. **How tinymold coexists with sccache and the shared `_workflow-shared`
   target dir** already in use on the development boxes, and with cargo's own
   upcoming cross-workspace cache and `-Zbuild-dir-new-layout`.
6. **Hash on Apple Silicon.** BLAKE3's native NEON path must be benchmarked on
   M-series hardware against SHA-256 with the SHA-2 extensions before the
   choice is final ([hashing §1.1](hashing-and-cache-server.md#11-throughput-landscape)).
7. **Page cache sharing.** Reflinked files do not share page cache [memory].
   Does that cost measurable time on the hot path, enough to prefer hardlinks
   for `.rlib`/`.rmeta` inputs?
8. **Benchmark corpus.** Which repositories (ripgrep, a Bevy app, a test-heavy
   workspace, a `-sys`-heavy workspace) represent the workloads we promise to
   improve?
9. **Platforms.** Linux and macOS first. Is Windows in scope, and when?
10. **Does content-defined chunking pay** for successive versions of the same
    large rlib and for test binaries? Measure before building.
