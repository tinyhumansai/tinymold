# OpenHuman in Docker

The corpus in `corpus.json` is small on purpose: projects that build in
seconds, so the scenario matrix runs in minutes. OpenHuman
([tinyhumansai/openhuman](https://github.com/tinyhumansai/openhuman)) is the
other end: about a thousand packages, a dozen vendored submodules, and one
large core library under a binary. This benchmark builds its `openhuman-core`
binary on Linux, in Docker, and compares three ways to build it:

| Config | Wrapper | Linker |
| --- | --- | --- |
| `ld` | none | GNU ld (rustc's default `cc` on aarch64 Linux) |
| `mold` | none | mold, via `-C link-arg=-fuse-ld=mold` |
| `tinymold` | `RUSTC_WRAPPER=tinymold` | mold |

Each config builds into its own `--target-dir`, so they never invalidate
each other.

## Cases

[`docker/openhuman.sh`](docker/openhuman.sh) runs these in order for each
config:

- **cold**: clean `target/`, and for tinymold an empty store. This is a
  first CI run or a fresh clone.
- **noop**: nothing changed.
- **edit_leaf**: a comment appended to the binary's `main.rs`. This is the
  hot edit loop at the top of the graph.
- **edit_core**: a comment appended to the core library's `lib.rs`, which
  recompiles the core and everything above it.
- **warm** (tinymold only): clean `target/` with the store from the builds
  above. This is a CI runner with a restored cache, or `cargo clean`.
- **second_path** (tinymold only): a copy of the sources at another path,
  clean `target/`. This is a second worktree or another agent's checkout.
- **snapshot** (tinymold only): the same copy, started from
  `tinymold snapshot restore` of the first checkout's `target/`.

`ld` and `mold` have no cache, so their warm and second-checkout builds are
their cold build; the report compares tinymold's cached cases against the
cold mold build.

Every build runs with `--timings`, so `bench/profile_build.py` can read the
critical path of any of them from `bench/.work/openhuman-docker/timings/`:

```sh
python3 bench/profile_build.py --from-html bench/.work/openhuman-docker/timings/mold-cold-1.html --jobs 12
```

## Reproduce

Requirements: Docker, `git` with access to the OpenHuman repository, and
Python 3.9 or newer.

```sh
python3 bench/docker_openhuman.py run --runs 3   # an hour or so for all three configs
python3 bench/docker_openhuman.py report         # rewrite the result file and print tables
```

`run` clones OpenHuman with its submodules into `bench/.work/openhuman`
(`--rev` pins a commit). It builds the
[`docker/Dockerfile`](docker/Dockerfile) image, with the toolchain OpenHuman
pins, its system libraries, mold, and tinymold built from this checkout. It
then copies the sources, without `.git` or `node_modules`, into a Docker
volume and runs every case. The sources live in a volume rather than a bind
mount, so builds use the VM's own filesystem and never cross into the host's.
Dependencies are fetched before the first build, so no build's time includes
the network.

The result is `bench/results/openhuman-docker-linux-<arch>.json`. It records
the Docker VM's CPUs, memory and kernel, the toolchain, the mold and ld
versions, and the filesystem the sources and store live on.

## What moves the numbers

- **Filesystem.** A Docker volume is ext4, which has no reflinks, so tinymold
  restores every output by copy. On btrfs, XFS or APFS a hit is a reflink and
  costs almost nothing.
- **The VM.** Docker Desktop's VM gets a share of the host's CPUs and memory.
  The result records what it had.
- **Build scripts and proc macros.** tinymold caches rustc invocations, not
  build-script runs, so every build script still runs on a warm cache. See
  [`PROFILING.md`](PROFILING.md).

## Findings

Linux aarch64 in Docker Desktop (12 CPUs, 16 GiB) on a darwin arm64 host,
OpenHuman 9a0a654, 2026-10-15. The table is in
[`README.md`](README.md#results); the raw samples are in
`results/openhuman-docker-linux-aarch64.json`.

- **A clean build is one crate long.** The core library (`openhuman`)
  compiles alone for 136s of a 175s clean build. Average concurrency is 2.3,
  and one unit runs alone for 139s. The build's 393s of unit time would fit
  in 33s on 12 slots. No linker or cache changes that first build: splitting
  the core crate, or a parallel frontend, is where its time is.
- **The linker decides the edit loop.** A one-line edit to the binary takes
  15.4s with GNU ld, 14.7s of it linking. With mold it takes 1.1s. An edit
  to the core library takes 32.9s with ld and 16.4s with mold; what remains
  is recompiling the core.
- **A warm cache turns a clean build into seconds.** With the store filled,
  a clean `target/` builds in 4.5s instead of 175s: 1410 of 1422 compiles
  hit, and the misses are build-script probes that fail by design. The
  critical path left is `libsqlite3-sys`'s build script (2.7s), which
  tinymold does not cache. Filling the store cost nothing measurable on the
  cold build (176s against 175s).
- **A new path misses about a quarter of the graph.** The first build of a
  copy at another path hit 347 of 474 compiles and took 165s. Repeat builds
  at that path then hit (5.8s). The first misses are crates with build
  scripts (`ref_cast`, `thiserror`, `serde_core`, `rustversion`), and the
  rest are their dependents. The likely cause, not yet confirmed: these
  crates read `OUT_DIR` through `env!`/`include!`, the key takes env values
  raw on purpose ([`rustc-wrapper.md`](../docs/specs/rustc-wrapper.md)), and
  outputs that record the path change their dependents' keys too. If so,
  remapping path prefixes so outputs stop depending on the checkout would
  make a new worktree warm.
- **A snapshot does not help a new path yet.** Restoring the first
  checkout's `target/` (4.5-9.6s, copies on ext4) still left the six
  workspace crates to recompile, all six missing, and that first build took
  155s. Presumably they key on the new path for the same reason.
- **Storing and restoring by copy costs the edit loop.** Without reflinks,
  storing the rebuilt binary costs about 0.7s on every edit, so a leaf edit
  takes 2.1s under tinymold against 1.1s with mold alone. A core edit takes
  19.1s against 16.4s: storing the core costs 1.5-3.4s, and the binary is
  then a hit, restored by a copy of 0.3-2.1s. On a reflinking filesystem
  both are close to free. On ext4, storing in the background would take
  them off the critical path.
