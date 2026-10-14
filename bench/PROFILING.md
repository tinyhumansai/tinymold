# Profiling a build

`bench.py` says how long a build takes. `profile_build.py` says where that
time went, so the gaps worth closing (serial chains, idle job slots, slow
build scripts, link time, wrapper overhead) show up with numbers attached.

```sh
python3 bench/profile_build.py                                  # ripgrep, clean, stable cargo
python3 bench/profile_build.py --case comment --nightly         # edit loop, with section timings
python3 bench/profile_build.py --mode tinymold --warm --nightly # every unit a cache hit
python3 bench/profile_build.py --case comment --self-profile    # plus rustc's own query profile
python3 bench/profile_build.py --from-html target/cargo-timings/cargo-timing.html
python3 -m unittest discover -s bench -p 'test_*.py'            # the analysis's tests
```

`--project` picks any corpus project and `--case` is one of `clean`, `noop`,
`comment`, `private_body`, or `public_signature`. Edit cases first build the
project, then apply the edit and profile the rebuild. `--from-html` analyzes
any existing cargo `--timings` report, from any project, without building.

## What it collects

| Source | What it gives | Needs |
| --- | --- | --- |
| `cargo build --timings` | every unit's start, duration, and the unit that unblocked it; concurrency; CPU use | stable cargo |
| `-Zsection-timings` | each unit split into frontend, codegen, and link, which places each `.rmeta` exactly | `--nightly` |
| link tracer | every linker call: start, end, and output | nothing (a Perl wrapper, like `bench.py`'s link timer) |
| `TINYMOLD_TRACE` | each wrapper step: `classify`, `pre_key`, `lookup`, `restore`, `rustc`, `ingest`, `pass_through` | `--mode tinymold` |
| `-Zself-profile` | rustc's own queries per crate, summarized and merged into a Chrome trace | `--self-profile`, plus measureme's `summarize` and `crox` |

Install the measureme tools with:

```sh
cargo install --git https://github.com/rust-lang/measureme --locked summarize crox
```

## What it reports

Everything lands in `bench/.work/profiles/<project>-<mode>-<case>/`:

- `report.md`, also printed: the wall-time split (cargo before the first
  unit, units, cargo after the last), parallelism, the critical path, the
  low-parallelism windows, the slowest units, linking, build scripts, proc
  macros, the wrapper's steps, and a ranked "where to look" list;
- `summary.json`: the same numbers, with the machine;
- `trace.json`: one Chrome trace of units on lanes (critical-path units
  tagged `critical`), their sections nested inside, links, wrapper steps, and
  concurrency and CPU counters. Open it in <https://ui.perfetto.dev>;
- `cargo-timing.html`: cargo's own report, kept for reference.

### Reading the critical path

Cargo records, for each unit, the units it was the *last* dependency of. So
every unit has one unblocker, and walking unblockers back from the unit that
finished last gives the chain that set the wall time. Per step:

- **Ready** is when its unblocker finished, or wrote its `.rmeta` across a
  pipelined edge. Units nothing unblocked are ready when cargo starts its
  first unit.
- **Queued** is the gap between ready and start: the unit waited for a job
  slot. Queued time on the critical path is time a smarter scheduler gets
  back.
- **On path** is the part of the unit the next step waited for: its whole run,
  or only up to its `.rmeta` across a pipelined edge.

Stable cargo does not report when a `.rmeta` was written, so without
`--nightly` a pipelined edge is assumed to have released its dependent the
moment the dependent started. The path is the same; only the split between
queued and on-path time is approximate.

The **lower bound** is cargo's head plus the larger of the critical path's
running time and total unit time over the job count. A build near its bound
can only get faster by shortening the critical path; one far above it is
losing time to scheduling.

### Caveats

- The wrapper's trace and the link tracer use the system clock; cargo's
  timeline is offset by when the profiler started cargo. They line up to a
  few milliseconds.
- Wrapper overhead is summed across concurrent invocations, so it can exceed
  its share of wall time.
- `--self-profile` sets `RUSTFLAGS`, which changes every unit's fingerprint;
  the primed build uses the same flags, so edit cases still rebuild only what
  the edit touched.
- A profile is one run. Use `bench.py` for medians before claiming a speed-up.

## Findings so far

Darwin arm64, 18 CPUs, ripgrep 14.1.1, 2026-10-14 (one run each; use them to
choose what to measure next, not as results):

- **Build-script runs dominate a clean build's critical path.** `libc`'s
  build script ran 2.6s of a 5.1s clean build, and build scripts held 63% of
  the critical path. With every rustc call a tinymold cache hit, build-script
  runs still held 95% of the path, because the wrapper caches compiles, not
  build-script runs.
- **macOS charges every new executable on first exec.** A fresh copy of
  ripgrep's build-script binary takes 0.13-0.23s to start the first time and
  under 0.01s after. With several build scripts starting at once, each run
  stretched to 1.2-1.8s. Restored and freshly linked build scripts and test
  binaries all pay it. Listing the terminal under System Settings > Privacy &
  Security > Developer Tools exempts processes it starts.
- **ripgrep's build script reruns on every edit.** It prints no
  `rerun-if-changed` on macOS, so cargo reruns it whenever a package file
  changes: 0.24s of a 0.64s comment-edit rebuild.
- **The edit loop's rustc time is mostly copying incremental objects.** In
  the comment edit, `rg`'s largest self-profile item was
  `codegen_copy_artifacts_from_incr_cache`: 868ms summed across threads, 63%
  of its profiled time, while the unit itself ran under 0.5s wall,
  for 256 codegen units.
