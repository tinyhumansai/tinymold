# Native link benchmark

`bench/native.py` reproduces the benchmark in
[mold's README](https://github.com/rui314/mold): the time to run the **final
link** of a large C/C++ debug build, once per linker, with everything before
that link already built. It answers "how fast is this linker on a multi-GiB
link" with the build system taken out of the measurement.

The corpus is pinned in [`corpus-native.json`](corpus-native.json):

| Project | Revision | Build | Link target |
| --- | --- | --- | --- |
| Clang | `llvmorg-23.1.3` | CMake + Ninja, `Debug`, `LLVM_ENABLE_PROJECTS=clang`, `LLVM_TARGETS_TO_BUILD=AArch64;X86` | `clang` (`bin/clang-23`) |
| Blender | `v5.2.2` | CMake + Ninja, `Debug`, precompiled libs via git-lfs | `blender` |
| Firefox | `FIREFOX_158_0_RELEASE` | `mach`, `--enable-debug --disable-optimize` | libxul (`XUL` on macOS) |
| Chromium | `149.0.7827.238` | gn + ninja, `is_debug=true symbol_level=2 is_component_build=false` | `chrome` (the `Chromium Framework` link on macOS) |

## What is measured

For each prepared project and each linker found on the machine:

1. The final link command is extracted from the build system:
   - CMake/Ninja and gn/ninja: `ninja -C <build> -t commands <target>`, keeping
     the last command that runs a compiler driver with `-o` and without `-c`
     (`tail -1` alone would pick CMake's `clang` symlink step). The ninja
     builds run with `-d keeprsp`, so response files referenced by the command
     survive the build.
   - Firefox: a dry run (`make -n -B`) of `obj/toolkit/library/build`, keeping
     the driver command whose `-o` names `XUL` or `libxul.so`.
2. CMake's `: && clang++ ... && :` wrapper and any `cd dir &&` prefix are
   stripped; the link runs directly (no shell) in the directory the build
   system would run it in.
3. The linker is swapped by inserting `--ld-path=<absolute path>` **right after
   the compiler-driver token**, after removing any `-fuse-ld=`/`--ld-path=` the
   build chose. (gcc's driver has no `--ld-path`; it gets `-fuse-ld=<name>`.)
4. One untimed warm-up run, then N timed runs. The output file is **deleted
   before every run**, as mold's methodology does. At the end the original
   command is re-run so the build tree is left as its build system made it.

Each result row records: project, revision (tag plus commit), build type,
linker name, path and version, compiler version, `link` (median, min, max, all
samples), output size, input count and bytes (objects, archives and libraries
named on the command line, in response files and in `-filelist` files),
output reuse (`deleted before every run`), the filesystem of the link
directory, and thread count (each linker's default; the CPU count is
recorded). Wild's author showed that output reuse, fork mode, filesystem and
thread count alone swing linker comparisons by up to 40%
([research](../docs/research/linkers.md)), so none of those are left implicit.

## Linkers

| Platform | Linkers measured when present |
| --- | --- |
| macOS | `ld-prime` (Xcode's default `ld`), `ld-classic` (Xcode's legacy ld64), `lld` (`ld64.lld` on `PATH`, else the toolchain's `rust-lld` via its `gcc-ld/ld64.lld` shim) |
| Linux | `bfd` (`ld.bfd`), `gold` (`ld.gold`), `lld` (`ld.lld`, else the `rust-lld` shim), `mold`, `wild` |

mold and wild do not link Mach-O, so macOS results compare Apple's linkers
against lld only. To include mold and wild, run on Linux with them on `PATH`
(`mold` from your distribution or its releases, `wild` via
`cargo install --locked wild-linker`). Restrict the set with
`--linkers lld,mold`.

## Running it

```sh
python3 bench/native.py prepare clang          # untimed: clone, configure, build
python3 bench/native.py link --runs 5 --project clang
python3 bench/native.py render                 # print the Markdown section
```

`prepare` is resumable: the clone is skipped when present, configure is
skipped when `build.ninja` exists, and the build itself is incremental. Run
`link` on an otherwise idle machine. Results merge into
`bench/results/native-<os>-<arch>.json` (re-running one project replaces only
its rows) and each project gets `bench/results/native-<os>-<arch>-<project>.svg`.
Everything is built under `bench/.work/native/<project>/`, which is git-ignored.

## Build recipes and cost

Measured on an Apple M5 Pro (18 cores, 48 GiB) unless marked as an estimate.

- **Clang.** Shallow clone of `llvm/llvm-project` at the tag (~2.5 GiB), then
  `cmake -G Ninja -S llvm -DCMAKE_BUILD_TYPE=Debug -DLLVM_ENABLE_PROJECTS=clang
  -DLLVM_TARGETS_TO_BUILD="AArch64;X86" -DLLVM_INCLUDE_TESTS=OFF
  -DLLVM_INCLUDE_BENCHMARKS=OFF` and `ninja clang`. Only two backends are
  built to keep the build tractable; mold's Clang number builds all targets,
  so its output (4.19 GiB) is larger than ours.
  Cost: CLANG_COST.
- **Blender.** Shallow clone from `projects.blender.org` at the tag, then
  `python3 build_files/utils/make_update.py --no-blender`, which fetches the
  precompiled libraries submodule (`lib/macos_arm64`, 1.3 GiB, git-lfs) without
  moving the pinned tag. Then `cmake -G Ninja -DCMAKE_BUILD_TYPE=Debug` and
  `ninja blender`. Needs a full Xcode on macOS. Cost: BLENDER_COST.
- **Firefox.** Shallow clone of `mozilla-firefox/firefox` at the tag. A
  generated `mozconfig` (`--enable-debug --disable-optimize`, objdir
  `bench/.work/native/firefox/obj`), then `./mach --no-interactive bootstrap
  --application-choice=browser` with `MOZBUILD_STATE_PATH` inside the work
  directory (the toolchains it downloads stay there), then `./mach build`.
  Estimate: 1–1.5 h build, 40–60 GB. Not yet run on this machine.
- **Chromium.** `depot_tools` (shallow), a hand-written `.gclient` pinned to
  `refs/tags/149.0.7827.238` so the sync can be `--no-history --shallow`,
  `gclient runhooks`, `gn gen out/Debug` with the args above, then
  `autoninja chrome`. `is_component_build=false` is required: debug builds
  default to component builds, which split the binary into hundreds of small
  links. On macOS the big link is `Chromium Framework`, which the `chrome`
  target pulls in. Needs a full Xcode with a recent SDK. Estimate: several
  hours (4–8 h on 18 cores), more than 100 GB of disk. Not yet run on this
  machine.

## Caveats

- Debug, not release: like mold's numbers, these links are dominated by debug
  info. On macOS debug info stays in the object files (no `.debug_*` copying
  into the output), so Mach-O outputs are much smaller than the same build's
  ELF output, and the link does proportionally less work than on Linux.
- Linking with a linker the project was not built for can fail (an lld-only
  flag passed to ld64, or the reverse). Failures are recorded per row with the
  linker's last error lines and shown as `failed` in the table, never dropped.
- The extracted command is the build system's, including its own flags
  (`-Wl,-dead_strip`, `-Wl,--gdb-index`, threading flags). Those are part of
  what the project actually asks its linker to do.

## Results so far

RESULTS_SO_FAR
