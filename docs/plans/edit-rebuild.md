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

### Linux, with mold and wild

`bench.py docker -- edits --steps 12 --churn-every 6 --fresh-every 4
--commands build,clippy,test --linkers default,mold,wild --gc 3`: Linux
aarch64 in Docker on the same Mac (12 CPUs, ext4 volume, so no reflinks),
with this work. Medians over edit steps; "fresh" is a build after the target
directory was deleted, three samples each.

| Project | Mode | Edit build | Test build | Fresh build | First edit after |
|---|---|---:|---:|---:|---:|
| ripgrep | cargo | 0.79 s | 0.61 s | 3.34–3.56 s | 0.72–0.85 s |
| ripgrep | tinymold | 0.85 s | 0.65 s | **0.13–0.15 s** | 0.77–0.96 s |
| ripgrep | cargo + mold | **0.28 s** | 0.27 s | 2.88–3.03 s | 0.25–0.38 s |
| ripgrep | tinymold + mold | 0.37 s | 0.31 s | **0.12–0.13 s** | 0.35–0.50 s |
| ripgrep | cargo + wild | **0.25 s** | 0.25 s | 2.84–3.02 s | 0.20–0.32 s |
| bat | cargo | 1.17 s | 1.62 s | 6.49–6.64 s | 1.13–1.32 s |
| bat | tinymold | 1.42 s | 0.94 s | **1.35–1.41 s** | 1.50–1.94 s |
| bat | cargo + mold | 0.37 s | 0.64 s | 5.29–5.67 s | 0.35–0.53 s |
| bat | tinymold + mold | 0.35 s | 0.34 s | **1.31–1.58 s** | 0.28–0.72 s |
| bat | tinymold + wild | **0.30 s** | **0.24 s** | 1.31–1.90 s | 0.26–0.56 s |

- **Is tinymold faster than mold?** They do different jobs. mold and wild make
  the link fast: the default GNU linker is two thirds of a ripgrep edit,
  and either takes the edit from 0.79 s to about 0.25–0.28 s. tinymold makes
  everything that did not change free: a fresh target directory builds in
  0.13 s instead of 3.4 s, and bat's test build drops from 1.62 s to 0.94 s.
  Combined, tinymold with wild gives bat's best edit (0.30 s), test build
  (0.24 s, 6.7 times faster than cargo) and a fresh build 4–5 times faster.
- **What tinymold still costs an edit:** 0.05–0.1 s over the same linker on
  ripgrep, the price of hashing and storing the relinked binaries.
- **The store on ext4 is a full copy**, so its growth is real disk. With
  `--keep-generations 3` it levels off within one configuration (bat
  2.14–2.21 GB from edit 3 to 5). But three generations of bat's test
  binaries, each with full DWARF on Linux, are large, and one public edit
  relinks them all (670 MB). Keeping fewer generations of linked
  executables than of libraries is the next lever.

## What is next

- **Prune target directories.** Target directories grow by 7–15 MiB an
  edit under tinymold, from churned configurations. Plain cargo leaves the
  same files behind but did not grow here, because it reuses one incremental
  directory per crate. A `tinymold prune` needs cargo's list of live units
  (`--message-format=json`) to remove the rest safely.
- **bat's gap** above.
- **Fewer generations for executables.** Linked binaries and test
  executables dominate store growth on Linux (above).
- **Linking.** A fast linker is the biggest single lever on Linux edits, and
  the doctor profile should recommend mold or wild. Only an incremental
  linker (`tinymold-link`) would take the link below mold's.
