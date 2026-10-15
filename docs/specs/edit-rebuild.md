# Edit rebuilds: fast, warm, and bounded on disk

- **Status:** Accepted
- **Owner:** Steven Enamakel
- **Date:** 2026-10-15
- **Plan:** [`../plans/edit-rebuild.md`](../plans/edit-rebuild.md)
- **Builds on:** [`rustc-wrapper.md`](rustc-wrapper.md), [`gc.md`](gc.md),
  [ADR 0008](../adr/0008-incremental-sessions-at-the-same-path.md), and the
  Scion notes in
  [`source-control-and-caching.md`](../research/source-control-and-caching.md#2-anthropic-redesigning-source-control-scion)

## Problem

A rebuild with no changes is already almost free under tinymold. What
developers, CI and agents repeat is the **edit loop**: change a few files,
then build, lint, test and measure coverage again. The first cumulative
edit benchmark (`bench.py edits`, 30 edits with churn, darwin arm64) found
four problems:

- **No gain on edits, and a loss on misses.** A private-body edit of the
  tinymold repository built in 0.56 s under tinymold against 0.48 s with
  plain cargo. A miss stored every output again, including the hundreds of
  objects incremental rustc had carried over unchanged.
- **A warm cache made the next edit colder.** A hit runs no rustc, so it
  leaves no incremental session. After a fresh target directory, the first
  edit took 0.76 s against plain cargo's 0.37 s on the tinymold repository,
  and 2.17 s against 0.78 s on ripgrep.
- **The store grew by every edit.** It went from 723 MB to 3,194 MB over 30
  edits of the tinymold repository, about 80 MB an edit: a new rlib, binary
  and test executables each time, and nothing ever removed them unless
  someone ran `tinymold gc`.
- **`cargo clippy` was uncached** for workspace crates, so every re-lint
  checked every crate again.

## Goals and non-goals

Goals:

- An edit under tinymold costs no more than under plain cargo, and the
  first edit after a fresh target directory at the same path costs about as
  much as any other edit.
- The store stays near a few generations of the live build, however many
  edits pile up, without anyone running a command.
- `cargo clippy` re-lints only the crates whose inputs changed.
- One benchmark shows all of it per edit: latency per command (build,
  clippy, tests, coverage), link time, peak RSS, and target directory and
  store growth. It runs for each linker, mold included, on Linux.

Non-goals:

- Faster linking itself. mold, wild and lld already do that, and tinymold
  works with any of them (see the Linux results). An incremental linker
  remains the `tinymold-link` research bet.
- Pruning stale units from target directories. Plain cargo grows target
  directories the same way. This is measured here, and left to a later spec.
- Sharing incremental sessions across worktrees, which rustc rejects
  (ADR 0008).

## Behavior

1. **Outputs carried over are not stored again** (`rustc-wrapper.md`, "On a
   miss"). An output whose identity matches the unit's placement record and
   whose mtime predates the compile takes its recorded digest.
2. **clippy-driver is cached** like rustc, with its version and the
   configuration file it would read in the pre-key (`rustc-wrapper.md`).
3. **gc keeps a bounded number of generations per unit**
   (`gc --keep-generations`), and **runs in the background** after wrapper
   calls, at most every 10 minutes, keeping three (`gc.md`, "Automatic
   collection").
4. **Incremental sessions travel with actions** and are restored on a hit
   into the directory they came from (`rustc-wrapper.md`, "Incremental
   sessions").

## Metrics

`bench.py edits` measures each of these per step, for each mode and linker:

| Metric | How |
|---|---|
| Latency | Wall time of each command: `build`, `clippy`, `test_build` (`cargo test --no-run`), `test_run`, `coverage` (`cargo llvm-cov`) |
| Link time | The timing linker wrapper; wild runs through clang and is not timed |
| Peak RSS | `ru_maxrss` from `wait4` over each command: its largest single process |
| Target and store size | Allocated blocks, each inode counted once; reflinked files count in full |
| Physical disk | Change in the volume's free space, the only measure that sees shared blocks; noisy |
| Store after gc | The store after `gc --keep-generations 3 --grace 0s` (with `--gc 3`) |

## Invariants

- No change here can produce a wrong hit. Carried-over digests trust the
  same identity rule the hit path already trusts. A session is a cache rustc
  validates. A collection only deletes.
- Nothing new runs on a hit's critical path except the session restore,
  which happens only when the unit's incremental directory is missing.

## Acceptance criteria

- On the edit benchmark, tinymold's median edit latency is no worse than
  plain cargo's, within noise, for build, clippy and tests.
- The first edit after a fresh target directory is within noise of a normal
  edit under tinymold.
- With `--gc 3`, the store after collection stays flat across edits, apart
  from churn steps that add a new configuration.
- A repeated `cargo clippy` with no change replays its lints from the store.
