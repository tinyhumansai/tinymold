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
