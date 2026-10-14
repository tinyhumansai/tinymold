# tinymold roadmap

> **Revised 2026-10-14** against the committed baselines in
> [`bench/README.md`](../../bench/README.md) and the second research pass
> ([`compilers-2026.md`](../research/compilers-2026.md)). Work is ordered by the
> seconds each phase removes from a measured row. Disk is cheap
> ([ADR 0005](../adr/0005-trade-disk-for-time.md)), so exit criteria gate on
> wall time, not bytes. Each phase gets its own spec in
> [`../specs/`](../specs/README.md) and a test-first plan in this directory
> before implementation starts. Every gate is measured with the existing
> harness in [`bench/`](../../bench/README.md), and before and after numbers
> go in the pull request.

- **Architecture:** [`../specs/architecture.md`](../specs/architecture.md)
- **Cache key:** [`../specs/cache-key.md`](../specs/cache-key.md)
- **Research and idea catalogue:** [`../research/README.md`](../research/README.md)

Every phase is released through the release workflow and published to
crates.io under the workspace version. New crates are published in the release
that first ships them.

## Phase 0: Foundations

**Goal.** A usable base that every later phase builds on, plus the design
documents that steer them.

**Deliverables.**

- `tinymold-hash`: the BLAKE3-256 `Digest` type, hashing of bytes, readers and
  files, hex encoding and parsing ([ADR 0002](../adr/0002-blake3-for-content-digests.md)).
- `tinymold-cas`: sharded local store, atomic insert, read-only objects, and
  materialization with a selectable strategy (reflink → hardlink → copy by
  default).
- `tinymold` CLI: `hash`, `cas put|get|has|verify`, and `doctor` (host, rustc
  version, fast linkers and sccache on `PATH`).
- Documentation: research reports and synthesis, architecture and cache-key
  specs, ADRs 0002 and 0003, this roadmap.
- Version 0.0.1 of `tinymold`, `tinymold-hash` and `tinymold-cas` published to
  crates.io by the release workflow, installable with `cargo install tinymold`.

**Exit criteria.** The change is merged with the four contract commands green,
and 0.0.1 is on crates.io.

**Dependencies.** None.

## Phase 1: Measure (done)

**Delivered.**

- `bench/bench.py` has these benchmarks, each with results committed for
  darwin arm64:
  - `baseline`, `cache` (sccache), `scenarios`, `dedup`, `link` and `cas`.
- The corpus is ripgrep, bat, tokio, bevy and tinymold itself.
- The `scenarios` benchmark pins jobs, filesystem, linker, output reuse and
  incremental, and records them with every result.

**Open items carried forward.**

- Linux baselines, including mold 3.0 and wild.
- Check whether the harness's `CARGO_INCREMENTAL=1` turns on incremental for
  registry dependencies too, which would inflate the clean-build rows
  ([compilers-2026 §9](../research/compilers-2026.md#9-what-tinymolds-baselines-say)).
- Add a `tinymold` mode to `scenarios` beside `baseline` and `sccache` once
  Phase 3 lands.

## Phase 2: Freshness and snapshots

**Status.** Implemented: see [`freshness.md`](freshness.md) for the results.

- Touch-all and second-worktree gates: met on ripgrep and tinymold.
- bat: one rebuild is unavoidable, because its binary depends on `.git`.
- Branch switching: compiles nothing on the return, but does not gain wall
  time yet.

**Goal.** Remove rebuilds caused only by mtimes and by a new path, without
caching individual units yet.

**Deliverables.**

- `tinymold fresh`: a content manifest of `(path → BLAKE3, mtime)` per
  workspace.
  - After a checkout, rebase, switch or touch, it restores the recorded mtime
    of every file whose content is unchanged.
  - It is backed by a stat cache keyed by
    `(dev, inode, size, mtime_ns, ctime_ns)` with git's racily-clean rule.
  - It can run from a `post-checkout` hook.
- A spike: `clonefile` (APFS) or reflink (Linux) of a warm `target/` into a
  second worktree, followed by `tinymold fresh`. It records which units cargo
  still rebuilds, and why.
- If the spike passes:
  - `tinymold snapshot save|restore`, keyed by
    `(commit, toolchain, profile, cargo config digest)`, keeping one snapshot
    per branch head;
  - `tinymold worktree-prepare`, which restores the nearest snapshot.

**Exit criteria.**

- The touch-all rebuild falls to no-op time: ≤ 0.12 s on ripgrep and bat,
  down from 0.84 s and 0.91 s.
- The second-worktree clean build takes ≤ 0.5 s on ripgrep and ≤ 1.5 s on
  bat, down from 3.8 s and 13.2 s.
- A branch switch A→B→A rebuilds nothing.
- A build from a snapshot produces outputs identical to a clean build, apart
  from differences the residual-path report explains.

**Dependencies.** Phase 0.

## Phase 3: Cache core

**Status.** First slice implemented. See [`rustc-wrapper.md`](rustc-wrapper.md)
for the results.

- Warm clean builds, branch switches and test builds beat both stock cargo
  and sccache on every corpus project.
- Filling the cache costs about 5%.
- Build-script runs, verify mode and background ingest remain.

**Goal.** Never compile the same unit twice on one machine, wherever it is
checked out, including the units sccache refuses.

**Deliverables.**

- **`tinymold-key`**:
  - ActionKey and ActionResult per the [cache-key spec](../specs/cache-key.md);
  - path normalization to `${WS}`, `${CARGO_HOME}`, `${SYSROOT}` and `${OUT}`;
  - proc-macro and build-script identity keys;
  - a golden-vector test for the encoding.
- **`tinymold-wrap`** as `RUSTC_WRAPPER`:
  - injects `--remap-path-prefix`;
  - looks up in two phases;
  - restores by reflink;
  - replays diagnostics;
  - prints `TINYMOLD_REMARKS=1` hit/miss lines.
- Caching for bin, test, cdylib and proc-macro units, and for build-script
  outputs.
- Cheap ingest: reflink into the store, hash on a background thread, write
  the ActionResult last, never compress.
- An action-cache layout aligned with cargo's proposal
  ([ADR 0004](../adr/0004-store-layout-aligned-with-cargo.md)), and support
  for build-dir layout v2.
- `tinymold explain`, which reads `-Zbuild-analysis` on nightly. A verify
  mode rebuilds and compares.

**Exit criteria.**

- Filling an empty cache costs ≤ 10% over an uncached clean build (sccache:
  +27% and +49%).
- A warm second worktree builds in ≤ 1.0 s (ripgrep) and ≤ 3.0 s (bat), where
  sccache takes 1.8 s and 8.9 s.
- A hit costs < 20 ms per unit, without a server.
- Verify mode on every benchmark hit finds zero mismatches.
- Every miss is attributed to a named key field.

**Dependencies.** Phase 2 (the stat cache).

## Phase 4: Test builds and links

**Goal.** Shrink `cargo test` builds after `cargo build`: 3.3 s on ripgrep
and 4.6 s on bat today.

**Deliverables.**

- `linker = tinymold`: link-skip keyed by input digests, arguments and linker
  identity. Linked binaries and test binaries are cached in the CAS.
- A speculative, low-priority `cargo build --tests` after each successful
  `cargo build`.
- A link concurrency governor for `cargo test`.
- Linker autoselect:
  - macOS keeps ld-prime and never uses rust-lld, which measured about 3x
    slower on bat;
  - Linux uses mold 3.x or wild when present.
- An advisor for consolidating test binaries, with an opt-in codemod.

**Exit criteria.**

- A warm build of tests after the binary takes ≤ 1.5 s (ripgrep) and ≤ 2.0 s
  (bat).
- A rebuild where files were only touched performs zero links.

**Dependencies.** Phase 3.

## Phase 5: Edit loop

**Status.** First experiments done. See [`edit-loop.md`](edit-loop.md).

- Debug-info levels, `-Zthreads` and Cranelift showed no consistent gain in
  sequential runs, which are too noisy to rule a gain out; none was A/B
  tested. Code generation is about a fifth of an edit.
- `codegen-units = 64` makes every edit kind 13-21% faster on ripgrep and bat
  in interleaved A/B runs, and is recommended.
- Early cutoff, prelinking and hot patching come next.

**Goal.** Cut the one-line edit rebuild: 0.52 s on ripgrep and 0.75 s on bat
today.

**Deliverables.** Each item is an experiment with a recorded result. Winners
become part of a reversible `tinymold doctor --fix` profile.

- A debuginfo profile: `line-tables-only` for deps, `split-debuginfo=unpacked`.
- Wider codegen sharding for the edited crate only, plus `-Zthreads` on
  nightly.
- Cranelift for workspace crates, on nightly where it is available.
- `hint-mostly-unused` per heavy dependency.
- Early cutoff on `.rmeta` digests, enabled per crate once its rmeta has been
  observed to be reproducible.
- A spike on a prelinked dependency image, followed by a Roc-style append
  link.

**Exit criteria.**

- The edit rebuild is ≥ 30% faster on the link-bound corpus project.
- A private-body edit rebuilds only that crate and its link, wherever rmeta
  is reproducible.

**Dependencies.** Phases 3 and 4.

## Phase 6: Hot server and remote

**Goal.** Share work across concurrent agents and machines. Speed on its own
matters less here: no-op builds are already 70-100 ms.

**Deliverables.**

- `tinymold-server`:
  - a Unix-socket daemon;
  - single-flight across worktrees and agents;
  - a stat cache and an FSEvents/inotify dirty set;
  - persistent state with snapshot boot.
- Prewarm: every `main` commit publishes cache entries and a `target/`
  snapshot.
- `tinymold-remote`: a REAPI v2 client, with remote writes only from trusted
  CI. **Landed ahead of the server**
  ([`remote-and-ci.md`](remote-and-ci.md)), together with last-use GC and
  the GitHub Action, because CI runners gain nothing from a per-machine
  daemon.

**Exit criteria.**

- Eight concurrent builds of one commit, in eight worktrees, compile each
  unit once.
- A warm hit has p50 ≤ 2 ms.
- Killing the server mid-build still produces a correct build.
- A fresh clone of `main` on a new machine reaches ≥ 90% remote hits.

**Dependencies.** Phase 3.

## Phase 7: Research bets

Each bet is a time-boxed spike that ends in an ADR:

- RDR-lite interface hashing (spiked: not on stable rustc, see
  [ADR 0007](../adr/0007-no-interface-hashing-in-the-wrapper.md));
- incremental linking contributed upstream to wild;
- a proc-macro expansion cache;
- a persistent `rustc_driver` worker;
- chunked storage of rlib members and DWARF sections.

Chunked storage is deferred by
[ADR 0005](../adr/0005-trade-disk-for-time.md). The dedup probe found 92%
sharing at member level against 79% at file level.
