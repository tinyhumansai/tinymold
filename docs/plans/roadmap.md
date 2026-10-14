# tinymold roadmap

> **Draft.** This roadmap is a starting point to be refined with the user in
> the planning step that follows. Phase boundaries, exit criteria and numbers
> will change. Each phase gets its own spec in [`../specs/`](../specs/README.md)
> and test-first plan in this directory before implementation starts.

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

## Phase 1: Measure

**Goal.** Know where time and bytes go on real projects before optimizing, and
be able to prove any later speedup.

**Deliverables.**

- `tinymold doctor` diagnosis: run or read `cargo build --timings` (and
  self-profile data on nightly) and classify a build as link-, frontend-,
  LLVM-, macro- or build-script-bound. Report `target/` size by artifact kind,
  flag churn (`RUSTFLAGS` differing between tools), feature-set divergence,
  and macOS hygiene issues (XProtect, Spotlight).
- `tinymold-bench`: a harness that pins filesystem, output reuse, linker fork
  mode and thread count and records them with every result. Scenarios follow
  rustc-perf: clean, no-op, comment edit, private body edit, signature edit,
  new worktree; metrics are wall time, link time, bytes in `target/`, and
  later hit rate.
- A chosen benchmark corpus (candidates: ripgrep, a Bevy app, a test-heavy
  workspace, a `-sys`-heavy workspace, tinymold itself) with baseline numbers
  committed under `docs/`.
- Design spec for the cache-miss explainer (`tinymold explain`).

**Exit criteria.**

- Baselines published for every corpus project on Linux and macOS.
- Benchmark medians repeat within ±5% across 5 runs on the same machine.
- `doctor` classification agrees with a manual reading of `--timings` on every
  corpus project.
- Explainer spec accepted.

**Dependencies.** Phase 0.

## Phase 2: Cache core

**Goal.** Never compile the same unit twice on one machine, across worktrees,
checkouts and branches.

**Deliverables.**

- `tinymold-key`: ActionKey and ActionResult per the cache-key spec, two-phase
  lookup, path normalization to `${WS}`, `${CARGO_HOME}`, `${SYSROOT}`,
  `${OUT}`.
- `tinymold-wrap` as `RUSTC_WRAPPER`: injects `--remap-path-prefix`, looks up
  and materializes hits from the local CAS, runs rustc on misses, ingests
  outputs atomically, replays diagnostics.
- Caching of the outputs sccache refuses: `bin`, test, `cdylib` and
  proc-macro crates, and build-script outputs.
- Checksum freshness on stable: restore mtimes from a content manifest after
  checkout, restore or `git switch`.
- `tinymold explain` and background verification sampling (default 1% of
  hits).
- A residual-absolute-path scan that refuses to share contaminated outputs.

**Exit criteria.**

- A second worktree of the same commit gets ≥ 90% unit hits on every corpus
  project, and its build is ≥ 5x faster than cold.
- Switching branches A → B → A rebuilds nothing on the return to A.
- Zero verification mismatches across the benchmark scenarios; any mismatch
  found is root-caused and either fixed in the key or made uncacheable.
- Every miss in the benchmark runs is attributed to a named key field by
  `tinymold explain`.
- Wrapper overhead on a hit is under 20 ms per unit without a server.

**Dependencies.** Phase 0; Phase 1 harness and explainer spec.

## Phase 3: Hot server

**Goal.** Make lookups nearly free, run identical work once across agents, and
bound disk use.

**Deliverables.**

- `tinymold-server`: Unix-socket daemon carrying keys and paths only;
  `tinymold-wrap` uses it when present and falls back to one-shot mode.
- Stat cache with the racily-clean rule; FSEvents/inotify dirty set with a
  full-walk fallback on overflow.
- Single-flight dedup of identical concurrent actions across worktrees and
  agents, with lease expiry.
- Per-key incremental-dir store swapped in on worktree or branch switch.
- Tiered storage (hot page-cache hints, warm CAS, cold zstd) with a global
  budget, per-project quotas, LRU eviction by last materialization, and a
  floor for each active workspace's latest graph.
- Persistent server state so restarts are warm; reflink-clone of a warm
  `target/` for new worktrees.

**Exit criteria.**

- Warm hit p50 ≤ 2 ms per unit, measured at the wrapper.
- No-op build overhead is proportional to changed files, not repo size
  (verified by scaling the corpus).
- 8 concurrent builds of one commit in 8 worktrees compile each unit once.
- 4 worktrees of a corpus project use ≤ 1.3x the disk of one, and total store
  size never exceeds the configured budget.
- Killing the server mid-build produces a correct build via fallback.

**Dependencies.** Phase 2.

## Phase 4: Linking

**Goal.** Shorten or remove the links that the cache cannot avoid.

**Deliverables.**

- `tinymold-link` and the wrapper's link mode: link-skip when inputs, args
  and linker are unchanged.
- Linker autoselect (wild, mold, lld, ld-prime) plus a debuginfo profile
  (`line-tables-only`, deps without debuginfo, unpacked split debuginfo, zstd
  debug sections on Linux if Phase 1 data supports it), applied as a
  reversible config patch and verified after the build.
- Link concurrency governor for `cargo test`.
- Test-binary consolidation (advisor, then opt-in codemod).
- Dylib dev mode: dependencies as one shared library for dev builds.
- Prelinked dependency archive stored in the CAS.

**Exit criteria.**

- A touch-only rebuild performs zero links.
- On the link-bound corpus project, the edit-run loop is ≥ 30% faster than the
  rust-lld baseline.
- `cargo test` peak RSS on the test-heavy corpus project stays under a set
  bound with no wall-time regression over 5%.
- Consolidation reduces the integration-test link count from N to 1 on the
  corpus project that has N test files.
- Dylib dev mode relinks the top crate in under 1 s on the Bevy corpus
  project.

**Dependencies.** Phase 2 (caching of link outputs); Phase 1 benchmarks.
Independent of Phase 3, so it can run in parallel with it.

## Phase 5: Early cutoff and remote

**Goal.** Stop edits from cascading through dependents, and share the cache
beyond one machine.

**Deliverables.**

- Early cutoff on rmeta digests: dependents keyed by upstream `.rmeta`
  content, enabled per crate once its rmeta has been observed to be
  reproducible.
- `tinymold-remote`: REAPI v2 client with zstd-compressed blobs and
  `FindMissingBlobs` batching; CI writes, developers read.
- CI pre-warm: every `main` commit publishes its cache entries for the
  default profile and targets.

**Exit criteria.**

- A comment-only or private-body edit in a crate rebuilds that crate and its
  link only; all dependents hit, on every corpus project where rmeta is
  reproducible.
- A fresh clone of `main` on a new machine reaches ≥ 90% hits from the remote
  tier.
- Works unmodified against bazel-remote and one other REAPI server.

**Dependencies.** Phases 2 and 3; reproducibility data from Phase 2
verification.

## Phase 6: Research bets

**Goal.** Explore the ideas with the highest ceiling and the most risk. Each
bet starts with a time-boxed spike and a go/no-go decision recorded as an ADR.

**Candidates.**

- **RDR-lite:** a span-insensitive hash of a crate's public interface from
  `.rmeta`, so body-only edits relink instead of rebuilding dependents.
- **Incremental linking with wild upstream:** contribute Rust-specific rlib
  diffing, state files or debuginfo handling to wild's incremental design.
- **Proc-macro expansion cache:** an allowlist of pure derives, using
  `-Zcache-proc-macros` on nightly or wrapper-level memoization, with
  periodic re-expand-and-diff verification.
- **Persistent `rustc_driver` worker:** a nightly opt-in that reuses
  process-level warm state across invocations.

**Exit criteria.** Each bet ends with a written result: measured gain on the
corpus, a soundness assessment, and either a spec for productizing it or an
ADR recording why not.

**Dependencies.** Phases 2-5 for measurement and infrastructure; upstream
coordination for the wild work.
