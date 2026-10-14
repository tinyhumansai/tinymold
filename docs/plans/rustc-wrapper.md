# Plan: the unit cache behind `RUSTC_WRAPPER=tinymold` (roadmap Phase 3)

- **Spec:** [`../specs/rustc-wrapper.md`](../specs/rustc-wrapper.md)
- **Status:** First slice implemented and measured. Build-script runs,
  verify mode and moving ingest off the critical path remain open.

## Order of implementation

1. **`crates/tinymold-key`**, pure functions with no I/O:
   - `Invocation::parse` (cacheable or pass-through, with canonical
     spellings);
   - `Roots` (per-worktree paths to `${CWD}`, `${OUT}`, `${PROFILE}` and
     `${CARGO_HOME}`, and back);
   - `DepInfo::parse`;
   - `pre_key` and `full_key`, pinned against golden vectors;
   - `content_digest` (static archives without their build stamps).
2. **`crates/tinymold-cas`:** `Strategy::Clone`, which tries reflink then
   copy and never hardlinks, for outputs a linker may rewrite.
3. **`crates/tinymold-wrap`:**
   - record formats (actions and pre-key input sets);
   - the stat cache, with git's racily-clean rule;
   - streaming exec that keeps cargo's pipelining;
   - `serve`: pre-key, lookup, restore, or run and ingest, with outputs
     ingested in parallel;
   - `run`, which falls back to rustc on any cache failure.
4. **`crates/tinymold`:**
   - wrapper detection: a first argument naming a program;
   - `TINYMOLD_LOG`;
   - an end-to-end test across two worktrees, an env-dep change, and a crate
     that bakes in `CARGO_MANIFEST_DIR`.
5. **Bench:** tinymold mode builds with the wrapper and reports hits and
   misses. Branch switching no longer uses snapshots. A run limited to some
   modes or cases replaces only those rows.

## Found by measuring, and fixed

- **Mismatched crates from outputs filtered by mtime.**
  - Incremental rustc hardlinks reused artifacts into the out-dir with their
    old mtimes, so an action could lack its `.rmeta`.
  - A later hit then left another branch's `.rmeta` beside a restored
    `.rlib`. rustc refused the final link of `tinymold` itself ("crate
    `tinymold_hash` required to be available in rlib format").
  - Fix:
    - outputs are every file with the unit's prefix;
    - the outputs `--emit` implies must be present;
    - a hit clears the unit's files first;
    - the schema moved to `tinymold/rustc/v2`, so v1 entries miss.
  - Regression tests: `outputs_rustc_reuses_from_its_incremental_cache_are_stored_too`
    and `a_hit_removes_stale_outputs_of_the_same_unit`. Both were confirmed
    to fail before the fix.
- **`-sys` crates missed on every clean build.** `ar` stamps archive members
  with the build time.
  - bat's `libgit2.a` was member-for-member identical across two builds once
    the stamps were zeroed.
  - Native libraries are now keyed by `content_digest`.
  - bat's warm clean build went from 121/131 to 130/131 hits. The remaining
    miss is anyhow's probe, which is meant to fail.
- **Filling the cache cost +12%** with serial ingest, because of the
  hundreds of incremental object files per crate. Parallel ingest, and stat
  entries only for files dependents hash, brought it to about +5-8%.

## Results

Command: `python3 bench/bench.py scenarios --runs 3 --modes baseline,sccache,tinymold`,
on darwin arm64 (18 CPUs, rustc 1.99.0, APFS), 2026-10-14, on a shared
machine with load averages of 3-13.

- tinymold mode builds with `RUSTC_WRAPPER=tinymold`, with `fresh` around
  each build and snapshots for new worktrees.
- Medians are shown; full tables are in
  [`bench/README.md`](../../bench/README.md).

| Case | Project | Baseline | sccache | tinymold |
|---|---|---:|---:|---:|
| Clean build, warm cache | ripgrep | 3.35 s | 1.89 s | **1.09 s** |
| | bat | 7.34 s | 11.13 s | **5.27 s** |
| | tinymold | 4.22 s | 2.07 s | **1.25 s** |
| Branch switch A→B→A | ripgrep | 1.18 s | 2.19 s | **0.43 s** |
| | bat | 1.72 s | 1.80 s | **1.44 s** |
| | tinymold | 1.32 s | 0.71 s | **0.49 s** |
| `cargo test --no-run` after a build | ripgrep | 2.82 s | 2.16 s | **0.29 s** |
| | bat | 2.25 s | 1.78 s | **0.71 s** |
| | tinymold | 2.75 s | 1.50 s | **0.55 s** |
| Second worktree, same commit | ripgrep | 3.39 s | 1.86 s | **0.08 s** |
| | bat | 7.17 s | 4.50 s | **1.55 s** |
| | tinymold | 4.17 s | 2.03 s | **0.07 s** |
| Comment edit | ripgrep | 0.53 s | 0.90 s | 0.55 s |
| | bat | 0.85 s | 0.68 s | 0.70 s |

**Filling an empty cache** with a clean build cost about 5% on ripgrep and
bat when runs were interleaved with plain builds. ripgrep took 3.21 s plain
and 3.53 s cold; bat about 7.2 s plain and 7.6 s cold.

- The scenario table's single "first (cold) run" samples swing with load, from
  4.1 s to 14 s for bat.
- Back-to-back cold builds of bat took 7.2-7.9 s.

**Hits are cheap.** A hit takes 1-80 ms per unit as logged, the larger values
for crates with hundreds of objects.

### What is left

- **Build-script runs.** cargo runs them directly, not through
  `RUSTC_WRAPPER`, so tinymold cannot see them. They dominate bat's warm clean
  build: about 1-2 s each for `libgit2-sys`, `onig_sys`, `semver`,
  `serde_json` and the rest.
- **Edits are not faster.** A miss is a miss. The edit loop is Phase 5's
  (sharding, early cutoff, linking).
- **A hit restores the debug-info objects of another worktree's compile,**
  whose debug map may name that worktree's paths. Builds are unaffected; a
  debugger may look for sources there.
- **Verify mode, and moving ingest to the background,** are open questions in
  the spec.
