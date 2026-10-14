# Plan: edit rebuilds that are fast, warm, and bounded on disk

- **Spec:** [`../specs/edit-rebuild.md`](../specs/edit-rebuild.md)
- **Decision:** [ADR 0008](../adr/0008-incremental-sessions-at-the-same-path.md)
- **Status:** Implemented. Results below; Linux results in
  [`bench/EDITS.md`](../../bench/EDITS.md).

## The idea, from Scion

Anthropic's Scion ([research notes](../research/source-control-and-caching.md#2-anthropic-redesigning-source-control-scion))
keeps a bounded cache on the client, makes every object immutable and
content-addressed, materializes on demand, and makes each operation cost
proportional to the change, not to the repository. Applied to builds:

- **Work proportional to the change.** A miss stores only the files rustc
  wrote, not the hundreds it carried over unchanged.
- **A bounded client cache.** The store keeps a few generations of each
  unit and collects the rest in the background, instead of growing with
  every edit.
- **Nothing lost by materializing from the cache.** A unit restored from the
  store brings its incremental session, so the next edit stays incremental.
- **The same content-addressed path for every command.** `cargo clippy`
  goes through the cache like `cargo build` already did.

## Order of work

Each step is a branch and a pull request, stacked in this order:

1. **Measure** (`edit-rebuild`): `bench.py edits`, the cumulative edit
   sequence with build, clippy, tests and coverage after each edit, sizes,
   peak RSS, `--linkers` (mold and wild on Linux through `bench.py docker`),
   `--gc` and `--against`. Tests in `bench/test_edits.py`.
2. **Carry outputs over** (`edit-ingest`): `carried_over` in
   `tinymold-wrap/src/compile`, `Store::keep` in `tinymold-cas`.
3. **Cache clippy-driver** (`clippy-cache`): `tinymold-wrap/src/driver`,
   the `driver` pre-key field in `tinymold-key`.
4. **Bound the store** (`store-generations`): `gc --keep-generations`,
   `--grace`, `tinymold_wrap::action_info`, and `tinymold/src/autogc`.
5. **Keep sessions** (`incremental-sessions`): `tinymold-wrap/src/session`,
   the `session`/`sfile` action lines, `Invocation::incremental`.

## Results

`bench.py edits --steps 20 --churn-every 7 --fresh-every 10 --gc 3 --against
<main's tinymold>`, darwin arm64, 18 CPUs, rustc 1.99.0, one run of 20
cumulative steps per project. `tinymold` is this work and `tinymold-base` is
`main` before it. Load from other work on the machine was high during the run,
so differences under about 10% are noise. Full tables are in
[`bench/EDITS.md`](../../bench/EDITS.md).

| Project | Measure | cargo | tinymold before | tinymold after |
|---|---|---:|---:|---:|
| tinymold | first edit after a fresh target | 0.79 s | 1.22 s | **0.79 s** |
| ripgrep | first edit after a fresh target | 0.79 s | 1.94 s | **0.86 s** |
| bat | first edit after a fresh target | 1.14 s | 1.83 s | 2.01 s (see below) |
| tinymold | store growth per edit | – | 80 MiB | **21 MiB** |
| ripgrep | store growth per edit | – | 46 MiB | **27 MiB** |
| bat | store growth per edit | – | 95 MiB | **41 MiB** |
| ripgrep | edit build (median) | 0.91 s | 1.06 s | **0.91 s** |
| ripgrep | coverage after an edit | 5.36 s | 4.97 s | **4.60 s** |
| bat | test build after an edit | 0.82 s | 0.53 s | **0.50 s** |

What the numbers say:

- **The cache no longer makes the next edit colder** on the tinymold
  repository and ripgrep: the first edit after a fresh target directory is
  back to plain cargo's time, where it was 1.5–2.5 times slower.
- **The store grows by a half to a quarter as much.** With
  `--keep-generations 3` it stays flat within one configuration: the
  tinymold repository's store after collection held 1,014–1,020 MiB from
  edit 3 to edit 6. It grows only at churn steps, where a new feature set
  adds a new generation of every unit it touches. The remaining growth is
  those configurations plus the stored sessions; age-based collection
  (`--max-age`) retires configurations nobody builds any more.
- **Edit builds and clippy cost about what they cost under cargo** (ripgrep
  0.91 s both, bat 0.99 s against 1.12 s, the tinymold repository 0.64 s
  against 0.60 s). Coverage is faster, because unchanged instrumented units
  hit. Test builds are faster on bat (0.50 s against 0.82 s) but still
  slower on the tinymold repository (0.66 s against 0.52 s). There every edit
  relinks test executables that the miss then hashes and stores in full.
- **Peak RSS is unchanged**: 331–425 MiB in every mode, set by rustc.

### Open: bat's first edit after a fresh target

On bat the first edit after a fresh target stayed slower than cargo's in the
benchmark. An interleaved repeat (`--steps 8 --fresh-every 2`, build only)
gave 1.27–1.83 s after, 1.42–2.32 s before, and 0.74–0.77 s for cargo.
Manual traces show the restored sessions working: rustc took 356–377 ms on
an edit of `src/bin/bat/main.rs` with a restored session, against 468–470 ms
without one and 313 ms warm. So the rest of the gap is elsewhere in that
build. It has not been isolated yet.

## What is next

- **Prune target directories.** Target directories grow by 7–15 MiB an
  edit under tinymold, from churned configurations. Plain cargo leaves the
  same files behind but did not grow here, because it reuses one incremental
  directory per crate. A `tinymold prune` needs cargo's list of live units
  (`--message-format=json`) to remove the rest safely.
- **bat's gap** above.
- **Linking.** On Linux the default linker is most of an edit (see
  `bench/EDITS.md`): mold or wild take ripgrep's edit build from 1.2 s to
  0.4 s, with or without tinymold. tinymold makes unchanged work free and
  mold makes the link fast; they add up. Only an incremental linker
  (`tinymold-link`) would go further.
