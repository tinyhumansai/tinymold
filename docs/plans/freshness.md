# Plan: content freshness and target snapshots (roadmap Phase 2)

- **Spec:** [`../specs/freshness.md`](../specs/freshness.md)
- **Status:** Implemented. The gates are met on ripgrep and tinymold; bat and
  branch switching fall short, for the reasons recorded below.

## Spike (done first)

The spike asked one question: does cargo accept a `target/` cloned from
another worktree? It was run on ripgrep 14.1.1, darwin arm64.

1. **Build worktree A, then clone its target into a second worktree B.**
   `cp -c -R` placed 200 MB and 456 files in 0.57 s. In B, cargo found 24
   registry crates fresh and recompiled all 9 workspace crates.
2. **Find out why B rebuilt.** The fingerprint JSON was byte-identical
   between A and B. The cause was mtimes alone: `clonefile` keeps A's output
   mtimes, and B's checkout is newer.
3. **Give B's files A's mtimes where the content matches.** B then had 33
   fresh units and 0 compiled, in 0.54 s, and the resulting binary ran
   correctly.
4. **Clone with one call.** A single `clonefile` on the whole directory took
   7 ms, against 0.57 s for `cp -c -R`.

That made the design: a content manifest that travels inside `target/`, and
whole-directory clones kept in the store.

## Order of implementation

1. **`tree`** (`crates/tinymold/src/tree/`): `clone_tree` uses one
   `clonefile` for the whole directory on Apple platforms. Elsewhere it walks
   the tree and reflinks or copies each file, preserving mtimes. Also
   `set_modified`.
2. **`fresh`** (`crates/tinymold/src/fresh/`):
   - the manifest encoding, with a pinned test;
   - `record`, which walks the workspace and reuses digests from the
     previous manifest;
   - `restore`, guarded by a target stamp.
3. **`snapshot`** (`crates/tinymold/src/snapshot/`):
   - `save`, which records and then clones into the store;
   - `restore`, which picks the best match, clones, and applies the manifest
     by content;
   - `list`.
4. **CLI** (`crates/tinymold/src/cli/`): `fresh record|restore`,
   `snapshot save|restore|list`, and `--workspace`/`--target-dir`. The
   target directory honours `CARGO_TARGET_DIR`.
5. **Bench:** a `tinymold` mode and a `touch_all` case in
   `bench/bench.py scenarios`.

## Found during measurement, and fixed

- **A stale-output hazard.** A snapshot restore originally left a file whose
  content differed from the snapshot with its old mtime. When that mtime was
  older than the snapshot's outputs, cargo treated stale outputs as current,
  which is a miscompile.
  - Fix: every differing file is now given an mtime newer than the outputs.
  - Snapshot restores always verify by content, never by mtime.
  - A snapshot whose manifest does not apply is removed and refused.
  - Regression tests: `a_file_that_differs_from_the_snapshot_is_made_newer_than_its_outputs`
    and `a_changed_file_older_than_the_outputs_is_made_newer`. Both were
    confirmed to fail without the fix.
- **A no-op cargo build rewrites `target/debug/<bin>.d`.** That moved a stamp
  computed over every output and made every later restore refuse. The stamp
  became a digest over cargo's fingerprint files only. It is exact, since it
  sees added, rewritten and deleted fingerprints, and it is cheap: a
  `fresh restore` on bat dropped from 44 ms to 14 ms.
- **`--replace` spent 0.27 s deleting the old target.** Now, when the old
  target's manifest is still valid, it is renamed into the store as an
  `auto-<fingerprint digest>` snapshot (ADR 0005: disk is cheap). That also
  makes switching back to it a restore.

## Results

Command: `python3 bench/bench.py scenarios --runs 3 --modes baseline,sccache,tinymold`,
on darwin arm64 (18 CPUs, rustc 1.99.0, APFS), 2026-10-14. The machine was
shared: the load average was 4-11 during the run. Medians, with units
compiled in parentheses; full tables are in
[`bench/README.md`](../../bench/README.md).

| Case | Project | Baseline | tinymold mode | Gate |
|---|---|---:|---:|---|
| Every file touched, no change | ripgrep | 0.86 s (9) | **0.08 s (0)** | ≤ 0.12 s: met |
| | tinymold | 0.40 s (3) | **0.07 s (0)** | met |
| | bat | 0.93 s (1) | 0.40 s (1) | missed: `.git` input, see below |
| Second worktree, same commit | ripgrep | 3.55 s (33) | **0.20 s (0)** | ≤ 0.5 s: met, 18x |
| | tinymold | 2.84 s (30) | **0.14 s (0)** | met, 20x |
| | bat | 13.84 s (109) | 1.81 s (1) | ≤ 1.5 s: missed, 7.6x |
| Branch switch A→B→A | ripgrep | 1.25 s (8) | 1.30 s (4) | nothing rebuilt on the return: met; wall time: no gain |
| | tinymold | 0.85 s (6) | 1.20 s (3) | regression |
| | bat | 1.88 s (2) | 3.29 s (2) | regression |

tinymold's own overhead per build is 8-30 ms for `fresh restore` plus
`fresh record`, and 60-250 ms for a snapshot restore into a new worktree.
Clean, edit, and test builds are unchanged, as expected: Phase 2 does not
cache units.

### What fell short, and why

- **bat recompiles its binary in every case.** bat's dep-info lists
  `.git/index` and `.git/logs/HEAD` as inputs, which `git` rewrites and
  which do not exist in a second worktree. tinymold never touches `.git`, so
  cargo is right to rebuild.
- **Branch switching does not pay yet.** Saving a snapshot right after a build
  has to wait for APFS to flush dirty data: 0.1-0.5 s, and up to 3.5 s after
  a full build. Branch B's own changes still compile. Together these cost
  more than the return trip saves.
  - Two fixes are possible: save in the background after each build, so the
    flush overlaps idle time, or rely on Phase 3's unit cache. Both are open
    questions in the spec.
  - Until one lands, use `snapshot restore --replace` for long-lived
    branches, not for quick round trips.

## Verification

- The four contract commands are green.
- Line coverage is at least 90% in every file. The new modules are at
  98-100%.
- `bench/README.md` was regenerated with `python3 bench/report.py`; the result
  JSON is committed with it.
