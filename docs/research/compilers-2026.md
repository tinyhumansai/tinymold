# Fast compilers and build caches, second pass (October 2026)

Date: 2026-10-14. Scope: what changed or was missed since the first five
reports. It covers mold 3.x, cargo's own shared-cache work, Swift/Clang
compilation caching, Bun's Zig fork, Roc's surgical linker, and a few
toolchain flags. It then reads tinymold's committed baselines to decide what
to build first.

Confidence labels: **[P]** = read in the primary source during this research;
**[S]** = secondary source only; **[M]** = from memory, not re-fetched.
Nothing here was reproduced by tinymold unless it is labelled **[measured]**.

## 1. mold 3.x

- 3.0.0 shipped on 2026-10-05. It is the first release written in Rust and
  works as a drop-in for 2.42.1, with linking speed "on par" with it. It is
  built with Cargo (Rust 1.95+), and oneTBB is gone. The 3.x goals are
  closing the remaining linker-script gaps with GNU ld and becoming a
  distribution's default linker
  ([releases](https://github.com/rui314/mold/releases)) [P].
- There is still no Mach-O target, so mold does not help on macOS.
- Incremental linking: the 2.42.1 notes say the project is "not against it"
  and that designs "are being explored", but faster full links made it less
  urgent [P].
- 2.42.0 added levels for `--compress-debug-sections`, discards temporary
  local symbols by default, and claims to be "noticeably faster" on
  many-core machines and as fast as or faster than lld and wild [P].
- Whether mold 3.x exposes a library API that tinymold could embed is
  unverified.

**For tinymold:** on Linux, autoselect still prefers mold or wild. On macOS,
the platform linker (ld-prime) stays. tinymold's own baseline measured
rust-lld at about 3x *slower* than the default linker for bat's edit loop on
darwin arm64 (3.50 s against 1.07 s) [measured], so autoselect must never
switch macOS to rust-lld.

## 2. Cargo's shared build cache is being designed now

- [cargo#5931](https://github.com/rust-lang/cargo/issues/5931), September 2026
  proposal [P]:
  - Entries live at `<build-cache>/entries/<pkg>/<build-unit-hash>` and point
    into a content store at `<build-cache>/content/`.
  - New entries are hardlinked into the content store, and the manifest is
    written atomically.
  - The prototype then hardlinks from the cache into the build dir.
- Prototype numbers (2026-09-11), with every non-local crate cached: about 20
  projects built in 6m46s against 12m59s on stable. The cache was 11 GB,
  against 31 GB of target directories [P].
- On 2026-10-06 the work moved toward a broader content-addressed design,
  starting with cross-workspace dedup
  ([cargo#17453](https://github.com/rust-lang/cargo/issues/17453)). There is
  no nightly flag yet, and the issue is "not yet accepted" [P].

**For tinymold:** this is the same two-level shape as
[ADR 0003](../adr/0003-two-level-content-addressed-cache.md). Aligning the
entry layout with cargo's ([ADR 0004](../adr/0004-store-layout-aligned-with-cargo.md))
lets tinymold become a superset of it, or a migration path to it, rather than
a competitor. The cargo design covers non-local crates only. Workspace crates
in other worktrees, link outputs, test binaries and build-script runs remain
tinymold's ground.

## 3. Build-dir layout v2 and build analysis

- **Layout v2** ([call for testing](https://blog.rust-lang.org/2026/03/13/call-for-testing-build-dir-layout-v2/),
  [nightly constant](https://doc.rust-lang.org/nightly/nightly-rustc/cargo/workspace/features/constant.STABILIZED_BUILD_DIR_NEW_LAYOUT.html)) [P]:
  - Each unit gets its own directory, `build/<pkg>/<hash>/{fingerprint,out}`.
  - A package with a build script gets separate directories for the script
    binary and for the script run; the run directory holds `out/` and `run/`.
  - `.cargo-lock` and `incremental/` stay at the profile level.
  - Nightly docs (September 2026) say the new layout is now always on. It is
    not on stable yet.
- **`-Zbuild-analysis`** ([unstable docs](https://doc.rust-lang.org/nightly/cargo/reference/unstable.html)) [P]:
  - It writes JSONL logs of timings and rebuild reasons to `$CARGO_HOME/log/`,
    which `cargo report` can query (cargo#15844).
  - This corrects the "unverified" note in
    [rustc-pipeline](rustc-pipeline.md).
  - The same page says `-Zembed-metadata=no` "will likely become the
    default", and adds `-Zsection-timings` (cargo#15817).

**For tinymold:**

- Under layout v2, a unit is one directory, so a restore or a garbage
  collection can act on whole `<pkg>/<hash>/` directories.
- On nightly, `tinymold explain` can read cargo's own rebuild reasons instead
  of inferring them.

## 4. Swift and Clang compilation caching (LLVM CAS)

Sources: [SwiftPM pitch, July 2026](https://forums.swift.org/t/pitch-compilation-caching-support-in-swiftpm/88079)
and the [LLVM CAS RFC](https://discourse.llvm.org/t/rfc-add-an-llvm-cas-library-and-experiment-with-fine-grained-caching-for-builds/59864) [P].

- **The key.** A dependency scan produces a content key from the sources, the
  command line and the imported modules.
- **Prefix mapping is on by default** whenever caching is on, so two
  checkouts share one key space.
- **Remote caching** plugs in through the LLVM CAS plugin API plus a gRPC path
  over a Unix socket. Without a plugin, the local on-disk CAS is used.
- **Hit/miss remarks.** A per-job hit or miss can be printed in the build log
  as a diagnostic remark.
- **Results.** A fully cached clean debug build was 67-75% faster than one
  with an empty cache (SwiftPM, SwiftSyntax, Vapor). Earlier Clang work
  (`-fdepscan`, `-fdepscan-prefix-map`) made rebuilds after a branch switch
  "often fully cached (~10s)".
- **Security warning.** The pitch says anyone who can write to the cache can
  tamper with compiler outputs.
- **Nothing equivalent exists inside rustc.** sccache, zccache and tinymold
  are the Rust equivalents.

### 4.1 The macro-plugin cache-key bug

From [Swift Forums, May 2026](https://forums.swift.org/t/compile-cache-key-instability-from-non-deterministic-macro-plugin-binaries/86695) [P].

- **The bug.** The plugin's bytes were part of each compile's key. A fresh
  `LC_UUID` and an ad-hoc `LC_CODE_SIGNATURE` on every link made about 3,747
  compiles miss between two otherwise identical builds.
- **Fixes discussed:**
  - `ld -reproducible`, the default since Xcode 26.4.1;
  - `-Wl,-no_uuid -Wl,-no_adhoc_codesign`;
  - keying on the plugin's *identity* instead of its bytes. A maintainer
    agreed identity is the right direction, but said the loaded plugin must
    still be checked against it.

**For tinymold:** the [cache-key spec](../specs/cache-key.md) keyed proc-macro
dylibs by their bytes, which has the same flaw on macOS. The spec now keys
proc-macro and build-script binaries by the ActionKey that produced them, and
checks the loaded bytes against that ActionKey's ActionResult.

## 5. Fine-grained object storage (MCCAS)

Sources: [2023 slides](https://www.llvm.org/devmtg/2023-10/slides/techtalks/Rastogi-RepresentingDebugInformationInLLVMCAS.pdf)
and [2024 slides](https://llvm.org/devmtg/2024-10/slides/techtalk/Rastogi-Fine-grained-compilation-caching-using-llvm-cas.pdf) [S].

- **What it does.** Objects are stored in the CAS in pieces smaller than a
  function. Across incremental builds, the store grows more slowly than
  ccache's.
- **Why it matters.** Debug info is about 90% of object bytes and largely
  redundant. llvm-project at `-O0` was 7.2 GB with `-g` against 0.74 GB
  without.
- **tinymold's own probe** [measured, tinymold 9db903e, darwin arm64]:

  | Granularity | Bytes byte-identical to the first worktree's |
  |---|---|
  | Whole files | 79% of 116 MiB |
  | rlib members | 92% of 43 MiB |
  | DWARF sections | 92% of 30 MiB |

**For tinymold:** chunking would buy about 13 more percentage points of
dedup. Disk is cheap ([ADR 0005](../adr/0005-trade-disk-for-time.md)), so
this is deferred until footprint, not time, is the constraint.

## 6. Bun and its Zig fork

Sources: [ziggit](https://ziggit.dev/t/bun-s-zig-fork-got-4x-faster-compilation-times/15183),
[devclass](https://www.devclass.com/software/2026/05/11/anthrophics-bun-team-trials-port-from-zig-to-rust/5237835),
[gist](https://gist.github.com/mbrock/26d70e708269e4cfa933d7943e0f0007) [S].

- **The speedup.** In April 2026, Bun's fork of Zig got debug builds about 4x
  faster from two changes: parallel semantic analysis, and splitting the
  output into many LLVM modules.
  - One measurement put 32 LLVM shards at about 37-42 s against 6m37s for a
    single module.
  - Zig core says the parallel analysis is non-deterministic, and will not
    take it upstream.
- **A Rust port.** In May 2026 the Bun team trialled porting from Zig to
  Rust, but says it is not committed to it.
- **"Buz".** This Zig fork of Bun reports sub-second incremental builds from
  binary patching. Only the Linux linker supports that.

**For tinymold:** rustc already shards codegen through codegen units. Wider
sharding on the crate being edited (more codegen units, plus `-Zthreads` on
nightly) is a cheap experiment. Cached units must stay deterministic, so the
experiment only applies to the crate being edited, never to what is shared.

## 7. Roc's surgical linker

Source: [README](https://git.joshthomas.dev/language-servers/roc/src/branch/main/crates/linker/README.md) [S].

- **How it works.** The host binary is preprocessed once: its GOT and PLT
  entries are removed and room is reserved for new sections. Each app build
  then only appends its own code and patches the call sites.
- **Limits.** The README lists Mach-O and PE as TODO.

**For tinymold:** this is the model for a "prelinked dependency image, then
append the workspace code" spike. Dependencies rarely change during an edit
loop, so linking them once and only appending the workspace crates is the
Rust analogue. It is research-grade on ELF and hard on Mach-O.

## 8. Toolchain flags and status

- **`hint-mostly-unused`**
  ([post](https://blog.rust-lang.org/inside-rust/2025/07/15/call-for-testing-hint-mostly-unused)) [P].
  - With the flag on one large dependency, a release build went from 4m07s
    to 2m04s and a dev build from 2m26s to 1m28s.
  - Crates can now declare it themselves in a `[hints] mostly-unused` table.
  - Cargo has a lint against applying it globally.
  - For tinymold: apply it per dependency, chosen from `--timings` data.
- **Parallel front end.** A 2026 goal that is accepted but not stable. It is
  waiting on a new test suite
  ([goal](https://goals.rust-lang.org/2026/parallel-front-end.html)) [P].
- **Cranelift.** Still the nightly `rustc-codegen-cranelift-preview`
  component. Whether it ships for aarch64-apple-darwin is unverified [P].
- **Zig.**
  - 0.15, still on LLVM, cut Ghostty's clean build from 41 s to 32 s and its
    incremental build from 19 s to 16 s
    ([Hashimoto](https://mitchellh.com/writing/zig-builds-getting-faster)) [P].
  - Zig 0.16's self-hosted x86_64 backend built the Zig compiler in 31 s
    against 2 min with LLVM [S].
- **Turbopack.** Memoization per function only works inside the compiler, so
  it does not carry over to a wrapper. Persistent caching was canary-only in
  2025 ([docs](https://nextjs.org/docs/15/app/api-reference/config/next-config-js/turbopackPersistentCaching)) [S].
- **Covered by others, not fetched.** Kotlin K2, Roslyn's compiler server and
  Sorbet's fast path [M] all come down to a warm daemon plus skipping work
  when the API is unchanged. The existing daemon and RDR sections already
  cover that.
- **Anthropic's Scion.** No public material beyond what
  [source-control-and-caching](source-control-and-caching.md) already covers.

## 9. What tinymold's baselines say

These are tinymold's own measurements on darwin arm64 (18 CPUs, rustc 1.99.0,
2026-10-14) [measured], from [`bench/README.md`](../../bench/README.md).

| Gap | ripgrep | bat | Reading |
|---|---:|---:|---|
| Build tests after the binary | 3.3 s | 4.6 s | The largest repeated cost after a clean build |
| Second worktree, clean build | 3.8 s | 13.2 s | Nothing is shared between worktrees today |
| Second worktree, sccache warm, every compile a hit | 1.8 s | 8.9 s | Links, build-script runs and copy-based restores remain |
| Filling an empty sccache | +0.9 s | +3.7 s | +27% and +49% overhead to fill the cache |
| Every file touched, content unchanged | 0.84 s | 0.91 s | Slower than a real one-line edit (0.52 s and 0.75 s); pure mtime noise |
| No-op | 0.07 s | 0.10 s | Already cheap; a daemon saves little here |

Three readings decide the order of work:

1. **Freshness, snapshots and caching beat compiler tricks.** The touch-all,
   second-worktree and test-build rows are wasted work. Removing them needs no
   rustc changes.
2. **sccache leaves most of the second-worktree cost on the table.** Even at
   100% hits, bat's second worktree spends 8.9 s. Links, build scripts and
   copies are where a better cache wins.
3. **Incremental compilation is expensive on a clean build.** tinymold's own
   clean build took 6.21 s with incremental on against 3.85 s with it off
   under sccache. Incremental should be off for anything cached and on only
   for the crate being edited.

   The baseline harness sets `CARGO_INCREMENTAL=1`. Cargo's default enables
   incremental only for workspace members, so whether that env var also
   turns it on for registry dependencies needs checking before quoting the
   clean-build rows as stock.

## 10. Ideas, ranked for tinymold

Impact: H = 2x or better on the targeted row; M = 15-50%; L = less than that.
Difficulty: S = days; M = weeks; L = months.

| # | Idea | Targets | Impact | Diff. |
|---|---|---|---|---|
| 1 | Content freshness: restore mtimes for content-identical files | Touch-all row | H | S |
| 2 | Clone `target/` snapshots (APFS `clonefile`, Linux reflink) into new worktrees and on branch switch | Second-worktree row | H | S-M |
| 3 | Unit cache that also covers bin, test, proc-macro and build-script outputs, restored by reflink | Second worktree, test builds | H | M |
| 4 | Key proc-macro and build-script binaries by identity; link them with `-Wl,-no_uuid` on macOS | Hit rate | M | S |
| 5 | Cheap ingest: reflink into the store, hash off the critical path, no compression | Cache-fill overhead | M | S |
| 6 | Link-skip, cached link outputs, speculative `--tests` prebuild | Test-build row | H | M |
| 7 | Entry layout aligned with cargo#5931; build-dir v2 aware | Future compatibility | L (strategic) | S |
| 8 | Per-unit hit/miss remarks; `explain` from `-Zbuild-analysis` | Trust | Enabler | S |
| 9 | Edit-loop experiments: debuginfo profile, wider codegen sharding, Cranelift, `hint-mostly-unused` | Edit row, clean row | M | S each |
| 10 | Prelinked dependency image, then a Roc-style append link | Edit row on link-bound projects | H if it works | L |
| 11 | Scion-style daemon: single-flight, snapshot boot, prewarm per `main` commit | Many agents | H for bursts | L |
| 12 | Chunked storage (rlib members, DWARF sections) | Disk | L under ADR 0005 | M |

The delivery order is in the [roadmap](../plans/roadmap.md).
