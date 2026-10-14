# Fast-tool engineering: Bun, Zig, esbuild, Go, tsgo — lessons for tinymold

Research date: 2026-10-14. Purpose: extract transferable principles for a Rust build accelerator ("tinymold").
Every number below is attributed; vendor benchmarks are vendor benchmarks and are labelled as such. Claims I
could not confirm from a primary source are marked **(unverified)**.

---

## 1. Executive summary

The fast tools agree on six points:

1. **Do less work, not the same work faster.** Zig skips LLVM for debug builds (~5x). esbuild makes 3 AST
   passes. Go reads only export data, never transitive sources. Bun reads binary manifests by offset instead
   of parsing JSON.
2. **Content-addressed caching with early cutoff.** Go keys outputs by an *action ID* (hash of all inputs).
   Downstream keys use the *content ID* of upstream outputs, so a rebuild that produces identical bytes stops
   the cascade.
3. **Memory layout is the main performance lever.** Struct-of-arrays, indices instead of pointers, one
   interned string buffer, arenas freed in bulk. Zig cut peak RSS ~3x; Bun's lockfile needs ~6 allocations.
4. **Count syscalls and process spawns.** Bun install makes 165k syscalls versus npm's 997k. It uses
   clonefile/hardlinks rather than copying, and presizes buffers.
5. **Keep a warm process.** Zig's `--watch` + `-fincremental` re-analyzes a 500k-line codebase in ~63 ms after
   an edit. A cold run takes ~14 s.
6. **Parallelism has to be designed in.** It has to cover parse and codegen, share memory, and run the serial
   link phase on its own fast path. tsgo reportedly gets ~3x from native code and ~3x from concurrency
   **(secondhand)**.

---

## 2. Bun

Sources: "Behind the scenes of bun install" <https://bun.com/blog/behind-the-scenes-of-bun-install>; Bun
bundler announcement <https://bun.com/blog/bun-bundler>; env docs
<https://bun.com/docs/runtime/environment-variables>.

### 2.1 Philosophy
- Bun treats installation as a **systems problem**. The post prices a user/kernel mode switch at 1,000–1,500
  cycles (~500 ns at 3 GHz) and designs to avoid them.
- It is written in Zig and makes direct syscalls (`openat`). It bypasses libuv's thread pool and JS
  marshalling.
- It is written from scratch on purpose, with a small feature surface "so it can stay fast". The bundler
  shares its parser with the runtime. Bun avoided IPC to an external bundler because "the overhead involved in
  interprocess communication would hurt performance".

### 2.2 Numbers (vendor benchmarks)
| Measure | bun | pnpm | npm | yarn |
|---|---|---|---|---|
| Install under strace (mean) | 5.6 s | 24.5 s | 37.2 s | 94.2 s |
| Total syscalls | 165,743 | 456,930 | 996,978 | 4,046,507 |
| futex calls | 762 | 116,577 | 663,158 | 2,499,660 |

- npm's futex count shows the cost of lock contention. Bun's work-stealing pool pushes with CAS
  (`@cmpxchgStrong`) and uses per-thread memory pools, which removes nearly all of that contention.
- `package.json` reads: 146,057 files/s (0.019 ms each) in Bun, versus 66,576/s in Node 24.
- Manifest resolution: bun takes 230 ms fresh and **9.1 ms cached**. npm takes 1.786 s fresh and 363 ms
  cached, so a cached npm run is slower than a fresh Bun run.
- Bundler: 10x three.js with sourcemaps and minify. Bun reports ~1.75x faster than esbuild, ~150x faster than
  Parcel 2 and ~220x faster than webpack.

### 2.3 Techniques
- **Binary manifest cache** (`~/.bun/install/cache/*.npm`). Strings are stored once in a shared buffer.
  Versions are fixed-size structs addressed by offset, so reading is pointer arithmetic with no parse step.
  Revalidation uses ETag / `If-None-Match`: a 304 confirms freshness without any parsing.
- **Lockfile as struct-of-arrays.** Packages, dependencies, versions and one `string_buffer` live in big
  contiguous arrays (~6 allocations total). One 64-byte cache line holds 8 eight-byte package structs. The
  post estimates ~1,200 cycles of pointer chasing to read a single dependency name in an object-graph design.
- **Binary lockfile was a mistake for humans.** `bun.lockb` was replaced by a text `bun.lock` because binary
  files cannot be reviewed or merged in PRs. The text format keeps the speed properties: deduplicated strings,
  dependency-ordered layout, and preallocation from the file size. *Lesson: a binary format is for caches, not
  for files that get committed.*
- **Copy backends, best first:**
  - macOS: `clonefile()` (APFS CoW), then `clonefile_each_dir`, then `copyfile`. The clone took 1.274 s versus
    2.955 s for the copy (2.32x).
  - Linux: hardlink, then `ioctl(FICLONE)` (btrfs/XFS reflink), then `copy_file_range`, then `sendfile`, then
    plain copy. Hardlink took 109 ms versus 325 ms for the copy (2.97x). Hardlinks fail across filesystems,
    which is why the fallback chain exists.
- **Presized buffers.** The gzip trailer's ISIZE (last 4 bytes) gives the uncompressed size, so Bun
  preallocates once when it is ≤64 MB. Inflation uses libdeflate (SIMD).
- **Overlapped latency.** Up to 64 concurrent HTTP connections run on dedicated network threads. On macOS, DNS
  is prefetched while `package.json` is still being parsed.
- **Arena allocation in the parser.** The transpiler parser allocates into one arena that is recycled after
  each parse. CONTRIBUTING.md tells contributors to prefer arenas over manual frees **(seen only in forks of
  CONTRIBUTING.md)**. The parser and bundler follow esbuild's architecture, and the bundler announcement says
  so explicitly.
- **Runtime transpiler cache.** It is content-addressable, so it never holds duplicate entries. It only caches
  files above a size threshold: 50 KB in older docs, 4 KB in newer ones. Bun's Docker images turn it off on
  ephemeral filesystems. *Lesson: caching small items costs more than it saves; set a size floor.*

---

## 3. Zig

Sources: 0.15.1 release notes <https://ziglang.org/download/0.15.1/release-notes.html>; 0.14.0 notes
<https://ziglang.org/download/0.14.0/release-notes.html>; devlog <https://ziglang.org/devlog/2025/>; "Zig's
new relationship with LLVM" <https://kristoff.it/blog/zig-new-relationship-llvm/>; "Goodbye C++"
<https://ziglang.org/news/goodbye-cpp/>; incremental internals (mlugg)
<https://mlugg.co.uk/posts/incremental-compilation-internals/> (403 to my fetcher; content taken from the HN
summary <https://news.ycombinator.com/item?id=49085666>).

### 3.1 Skip LLVM for debug builds
- In Zig 0.6, the LLVM step took "at least 70% of the total compilation time" even in debug builds without
  optimizations (kristoff.it).
- In 0.15, the self-hosted x86_64 backend is the **default in Debug** (except on Windows and the BSDs). The
  notes report "around a 5x decrease" in compile time.
  - `hello.zig`: 918 ms down to 275 ms. Peak memory: 214 MB down to 137 MB.
  - Building the Zig compiler: **75 s down to 20 s**. The team describes it as going from "1–2 minutes" to "a
    few seconds".
  - Trade-off: the backend "emits slower machine code". It passes 1984 of 2008 behavior tests, against 1977
    for LLVM.
- *Rust parallel:* the Cranelift backend (`rustc_codegen_cranelift`). The 2025H2 project goal measured ~20%
  less codegen time on Zed, Tauri and hickory-dns, which is only ~5% of a clean build
  (<https://rust-lang.github.io/goals/2025h2/production-ready-cranelift.html>). Rust gains less from a fast
  backend than Zig does because rustc's frontend, monomorphization and linking take a much bigger share of the
  time.

### 3.2 Threaded pipeline
- Since mid-2025: one semantic-analysis thread, N codegen threads and one linker thread run concurrently.
  Gains are 5–50% wall clock. Compiler build: 13.8 s down to 10.0 s (−27%). Hello world: 355 ms down to 244
  ms.
- Once codegen is parallel, "the linker is now sometimes the bottleneck". The next target after codegen is
  always the serial link step.

### 3.3 Incremental compilation and in-place binary patching
- The design comes from kristoff.it (2020). The compiler **stays resident**. The output binary is laid out as
  loosely coupled blocks, one per top-level declaration, with slack space. Calls go through a GOT so moved
  functions need no callsite fixups. A changed function is rewritten **in place in the executable**, with no
  relink. DWARF line info is patched too, and Zig proposed a new DWARF opcode to support it. This has to be
  implemented separately for each object format (ELF, MachO, PE, Wasm). Forum users reported ~0.5 ms updates
  in early prototypes **(anecdotal)**.
- Dependency model:
  - The unit of work is the **analysis unit** (decl/function), not the file.
  - Dependencies track separate properties of a declaration: its layout, type, value and body. Changing only a
    function *body* does not invalidate callers that depend on its *type*.
  - ZIR (untyped IR, produced per file) is diffed between versions, and *tracked instruction indices* map old
    declarations to new ones.
  - The InternPool, the single global store of types and values, holds stable tracked indices for this purpose
    (Jan 2024 commit).
  - A debug flag serialized the InternPool to a 43 MiB `.zcs` file. State serialization is still unfinished,
    so for now incrementality only works with `--watch` (a warm process).
- Measured: 0.14 notes, ~500k-LOC codebase. The first check took ~14 s; an update after an edit took **~63
  ms** (`-fincremental --watch -fno-emit-bin`). The watcher debounces at 50 ms by default. On macOS it uses
  FSEvents (0.15).
- Status in 0.15: still experimental, with "known bugs" that "can lead to miscompilations". *Lesson:
  correctness is the hard part of incrementality. Ship a verify mode.*

### 3.4 Data-oriented design (DoD)
- Andrew Kelley's "Practical DoD" talk applies the idea to compilers. Tokens and AST nodes are stored as
  struct-of-arrays (`MultiArrayList`), nodes refer to each other by `u32` index rather than pointer, and
  padding is eliminated.
  - Commit history: tokens shrank from 64 B to 13 B (stage1 rework) and to ~5 B in the stage2 design (tag plus
    byte offset).
  - Moving token tags into their own array cut cache misses 2.67M to 1.76M (−34%) for ~4% fewer cycles.
- Pipeline IRs: AST, then ZIR (untyped, per file, cacheable and embarrassingly parallel), then Sema, then AIR
  (typed, per function), then MIR/machine code. Each IR is a flat, compact array.
- Results ("Goodbye C++"): the self-hosted compiler builds itself with **3.8 GiB peak RSS versus 11.3 GiB**
  for the C++ stage1 (Debug), and 3.2 versus 10.3 GiB (Release). Stage1 had the D-style "never free until
  exit" design, which stopped scaling.
- The Zig cache directory stores per-file ZIR keyed by content hash, so unchanged files skip parsing and
  AstGen entirely.

---

## 4. esbuild

Source: FAQ "Why is esbuild fast?" <https://esbuild.github.io/faq/>.

- **Native code.** A CLI is "a worst-case performance situation for a JIT-compiled language": the VM re-parses
  and re-JITs the tool on every run. For tinymold: a short-lived helper process must be AOT-native and start
  instantly, or else live in a daemon.
- **Shared-memory parallelism.** The phases are parse (parallel per file), link (mostly serial) and codegen
  (parallel per chunk). JS workers that serialize data between heaps get roughly half the parallel benefit.
- **Exactly three AST passes:**
  1. lex, parse, scope setup and symbol declaration;
  2. binding, syntax minification, TS/JSX lowering and ES lowering;
  3. identifier minification, codegen and sourcemaps.

  Few representation changes means few full walks over memory.
- **Compact data.** Typed fields, 1-byte bools and inline structs.
- **Written from scratch,** so data structures stay consistent end to end. No translating between third-party
  ASTs.
- Numbers (vendor, 6-core 2019 MBP), 10x three.js (547k lines):

  | Bundler | Time | Throughput |
  |---|---|---|
  | esbuild | 0.39 s | 1.40M LOC/s |
  | Parcel 2 | 14.9 s | |
  | Rollup + terser | 34.1 s | |
  | webpack 5 | 41.2 s | |

  Old Rome TS codebase (132k lines): esbuild 0.10 s, webpack 16.7 s.
- "Each factor gives a modest speedup, but together…multiple orders of magnitude". *There is no single trick;
  the multipliers compound.*

---

## 5. Go: the compiler and the build cache

Sources: `go help cache` / cmd/go docs <https://pkg.go.dev/cmd/go#hdr-Build_and_test_caching>; buildid.go
comment <https://go.dev/src/cmd/go/internal/work/buildid.go>; "Go at Google"
<https://go.dev/talks/2012/splash.article>.

### 5.1 Language-level choices
- **Export data instead of headers.** If A imports B and B imports C, the object file for B carries everything
  about C that A needs. Compiling A never reads C. The export section comes first so the reader can stop
  early. Pike measured ~40x include fanout at Google for Go versus ~2000x for C++.
- **No import cycles.** The dependency graph is a DAG by construction, which "simplifies compilation, linking,
  initialization".
- *Rust analogue:* `.rmeta` is Rust's export data. Cargo's pipelining already starts dependents as soon as
  `.rmeta` is ready. But rmeta contains *all* metadata, spans included, so it changes on almost any edit. The
  "Relink, don't rebuild" goal and `-Z public-api-hash` (PR #155871, April 2026) attack this. That PR is
  described as a coarse baseline, and the 2025H2 goal was marked "will not complete"
  (<https://rust-lang.github.io/goals/2025h2/relink-dont-rebuild.html>,
  <https://github.com/rust-lang/rust/pull/155871>).

### 5.2 Build cache design (the model to copy)
- **Action ID** = hash of *all* inputs to a step: source contents, flags, tool IDs (a compiler change
  invalidates everything), and the *content IDs* of dependency outputs.
- **Content ID** = hash of the produced output. It is embedded in the artifact's build ID, so it can be read
  cheaply without rehashing.
- **Early cutoff.** Because downstream action IDs fold in upstream *content* IDs and not upstream action IDs,
  a rebuilt dependency that produces byte-identical output leaves every dependent's key unchanged, and the
  dependents are skipped.
- Content-addressed `GOCACHE`, with automatic trimming of entries not used recently.
- `GOCACHEPROG` lets an external program provide the cache, for remote or shared caches.
- Introspection tools: `GODEBUG=gocachehash=1` prints every input to a key, and `gocacheverify=1` rebuilds and
  checks the result against the cache. These are essential for debugging keys.
- **Test result caching.** A passing result is reused when the test binary and the "cacheable" flags match,
  *and* any files the test opened and env vars it read are unchanged. Go tracks those at runtime. Only passes
  are cached.
- Known hole: cgo C library changes are not detected. This is the same hole as Rust's `build.rs` and linking
  against native libraries.

---

## 6. TypeScript-Go (tsgo)

Sources: <https://devblogs.microsoft.com/typescript/typescript-native-port/>; "Why Go?" discussion
<https://github.com/microsoft/typescript-go/discussions/411>.

| Codebase | LOC | tsc (JS) | tsgo | Speedup |
|---|---|---|---|---|
| VS Code | 1,505,000 | 77.8 s | 7.5 s | 10.4x |
| Playwright | 356,000 | 11.1 s | 1.1 s | 10.1x |
| TypeORM | 270,000 | 17.5 s | 1.3 s | 13.5x |
| date-fns | 104,000 | 6.5 s | 0.7 s | 9.5x |

- Memory is roughly half. Editor project load for VS Code: 9.6 s down to 1.2 s (8x).
- **Why Go:** it was a *port, not a rewrite*. Code structure and semantics stay close to the original, and
  "Idiomatic Go strongly resembles the existing coding patterns". GC was acceptable, and "Batch compilations
  can effectively forego garbage collection entirely, since the process terminates at the end." Go was also
  judged ergonomic for polymorphic tree walks in both directions.
- Breakdown: about ~3x from native code times ~3x from shared-memory parallelism **(secondhand, attributed to
  Hejlsberg in discussion #411)**.
- *Lesson for tinymold:* (a) one-shot processes can skip freeing memory altogether; arena or leak-on-exit is a
  valid strategy. (b) Shared-memory concurrency over an immutable graph is half the win.

---

## 7. The Rust baseline tinymold works against

- **Cargo freshness** combines fingerprints with file **mtimes** in `target/`, per crate. An mtime bump with
  identical content still triggers a rebuild, as with `git checkout` or a fresh worktree. Reverse dependencies
  rebuild whenever upstream rmeta changes, and almost any edit changes it.
- **rustc incremental** has query-level red/green early cutoff, but its state lives in a per-target-dir
  `incremental/` directory. It is not content-addressed, not shareable, and large.
- **sccache** cannot cache crates that invoke the linker (bin, dylib, cdylib, proc-macro) or incrementally
  compiled crates. Those are usually exactly the workspace crates a developer is editing
  (<https://github.com/mozilla/sccache/blob/main/docs/Rust.md>). It also misses `env!` and file reads made by
  proc macros.
- **Parallel frontend** (`-Z threads=8`): up to ~50% best case, ~30% aggregate
  (<https://blog.rust-lang.org/2023/11/09/parallel-rustc/>).
- **Linkers.**
  - Wild README (vendor): ~80 MB binary links in 363 ms with wild, 457 ms with mold, 905 ms with lld and 12.3
    s with GNU ld.
  - Zed's measurements: an incremental rebuild takes 3.9 s with wild, 5.0 s with mold and 6.6 s with lld
    (<https://github.com/davidlattimore/wild>).
  - No production Rust linker does incremental or in-place patching yet. The link is the serial tail of every
    edit-compile-run loop.
- Rust 2026 "Fast Builds" roadmap (<https://rust-lang.github.io/goals/2026/roadmap-fast-builds.html>) lists
  RDR, Cranelift incremental and related work. Tinymold should complement that roadmap rather than duplicate
  it.

---

## 8. Transferable principles

### 8.1 What to cache
| Artifact | Why | Precedent |
|---|---|---|
| Crate outputs (`.rlib`/`.rmeta`) | Most common hit | Go GOCACHE, sccache |
| **Linked-crate outputs** (bin/cdylib/proc-macro .so) | sccache gap | Go caches linker output under the main package's action ID |
| Build-script *outputs* (`OUT_DIR`, `cargo:` lines) | build.rs reruns are slow and often dominate | Go test cache plus tracked file/env reads |
| Proc-macro expansions | Hot in serde/derive-heavy code | rustc `-Z cache-proc-macros` (unstable) |
| Test results | Skip unchanged passing tests | `go test` cache |
| Resolved metadata (dependency graph, `cargo metadata`) | Cargo re-resolves on each invocation | Bun binary manifest cache |
| Public-API hash per crate | Enables early cutoff downstream | Go content ID, Zig per-property deps |

### 8.2 How to key the cache
- **Key on content, never mtime.** Hash source bytes (BLAKE3/xxh3), then memoize hashes by (inode, size,
  mtime, ctime) so unchanged files are not re-read. That is the Bazel/Buck2 approach.
- The key must include: rustc version plus host/target, codegen-relevant flags (with path-remapping flags
  normalized), the feature set, `RUSTFLAGS`, the profile, dependency **output** content IDs (not their keys),
  the env vars actually read (`env!`/`option_env!` via dep-info), and files read by build scripts and proc
  macros (dep-info `# env-dep:` plus `rerun-if-*`).
- **Two-level IDs, as in Go.** The action ID decides whether a step can be skipped. The content ID is what
  dependents hash, and it gives early cutoff for free when the output bytes do not change.
- **Path independence.** Use `--remap-path-prefix` / `-Zremap-path-scope` so worktrees and CI share entries.
  Otherwise every worktree fragments the cache.
- **Ship a `--explain`/`verify` mode,** modelled on `gocachehash` and `gocacheverify`. A cache nobody can
  debug gets switched off.

### 8.3 Avoid redundant work
- **Interface hashing.** Hash only rmeta items that affect dependents: the public API, generics, inline fns,
  type layouts, `#[macro_export]`. A change that leaves that hash unchanged lets tinymold skip dependents'
  re-check. When the public interface is stable, it can relink instead of rebuilding (the RDR idea, done
  outside rustc). Be conservative: private fields affect `size_of`.
- **Do not re-resolve.** Keep the graph in memory (daemon) or in an mmap'd binary snapshot.
- **Size floor.** Do not cache tiny steps where hashing and I/O cost more than rerunning (Bun's 4–50 KB
  threshold).
- **Debug-only fast paths.** Cranelift for workspace crates, LLVM for dependencies (cached once). Zig's own
  trade-off was slower code for 5x faster builds.

### 8.4 Process and memory layout
- **SoA graph store.** Crates, units and edges in flat arrays with `u32` indices, plus one interned string
  table. This is Bun's ~6-allocation lockfile and Zig's MultiArrayList.
- **Arena per invocation, or leak-on-exit,** for one-shot CLIs (tsgo).
- **Lock-free work stealing with per-thread allocators.** Bun's futex count was 762 against npm's 663k.
- **Binary on-disk indexes** read by offset (Bun manifest). Keep committed or human-facing files as text
  (`bun.lockb` to `bun.lock`).

### 8.5 Filesystem tricks
- **Materializing cache hits:** clonefile (APFS), then FICLONE (btrfs/XFS), then hardlink, then
  `copy_file_range`, then copy. Bun measured 2.3–3x over plain copy. Note the order difference: Bun prefers
  hardlinks on Linux, but hardlinks are unsafe for outputs a tool may later rewrite in place. rustc/cargo can
  rewrite outputs, so prefer reflinks; use hardlinks only for files that are read-only by contract.
- **mmap** cache indexes and large rlibs instead of reading them. Hash through the mapping.
- **Fewer syscalls.** Batch stat calls (`getattrlistbulk` on macOS, `statx` on Linux), keep directory fds open
  with `openat`, and presize buffers (gzip ISIZE trick). Use FSEvents/inotify instead of stat-walking
  `target/`.
- Never put build output on tmpfs `/tmp` (RAM-charged). Keep a single shared target directory per box,
  consistent with the existing local setup (`_workflow-shared` plus sccache).

### 8.6 Daemon versus one-shot
- **Warm state wins by orders of magnitude.** Zig: 14 s cold versus 63 ms warm. Bun: 9 ms cached manifests.
- **Hybrid model (recommended).** A thin, native CLI (esbuild's "no JIT" point) talks to a per-workspace
  daemon over a Unix socket. The daemon holds the resolved graph, file-hash memo, watcher state and the hot
  cache index.
- **Every cache must also persist to disk.** Zig is still blocked on state serialization, so its
  incrementality only works while `--watch` runs. Do not repeat that: design the daemon's state to be
  serializable from day one.
- **Degrade to one-shot** when no daemon is running. Never make correctness depend on the daemon.

---

## 9. Concrete ideas for tinymold

Impact is the expected wall-clock gain on the stated workload. Difficulty is S/M/L/XL.

| # | Idea | What | Expected impact | Diff. |
|---|---|---|---|---|
| 1 | Content-hash freshness layer | Memoize file hashes by inode/size/mtime; a touched file with identical content stays fresh (restore the mtimes cargo expects) | Ends spurious full rebuilds after checkout, rebase or a new worktree; minutes become seconds | M |
| 2 | Go-style action ID / content ID cache as `RUSTC_WRAPPER` | Dependent keys hash upstream *output* content | Dependents skip when a rebuilt crate's rmeta is byte-identical; big with #3 | M–L |
| 3 | Public-interface hash ("relink, don't rebuild" outside rustc) | Stable hash of public API, inline/generic bodies and layouts from rmeta (or `-Z public-api-hash`); unchanged means reuse dependents and relink only | Edit-to-run goes from O(dependents) to O(1) crates plus a link; maybe 5–10x on leaf-to-root edits | XL (soundness: layouts, macros; needs verify mode) |
| 4 | Cache what sccache cannot: bin, cdylib, proc-macro outputs and link results | Key the link by object/rlib content IDs, linker args and linker version | CI and fresh worktrees skip links of unchanged binaries; 20–60% of no-op time in bin-heavy repos | M |
| 5 | Build-script output cache | Hash build.rs, `rerun-if-*` inputs, env and compiler; replay `OUT_DIR` and stdout directives | Removes repeated `*-sys`, bindgen and prost runs, often the slowest serial cold step | M (untracked side effects need a sandbox or allowlist) |
| 6 | Reflink-first materialization | clonefile, then FICLONE, then `copy_file_range`, then copy; hardlink tier only for immutable rlibs | 2–3x faster restores (Bun's numbers); near-zero extra disk on APFS/btrfs/XFS | S |
| 7 | Path-normalized, worktree-shared cache | `--remap-path-prefix` plus key normalization | New worktree builds in seconds instead of minutes; fits the per-PR worktree workflow | S–M |
| 8 | Resident daemon with a serializable graph snapshot | mmap'd SoA graph (units, edges, interned strings, hash memo), FSEvents/inotify watcher, one socket round trip from the CLI | No-op build drops from hundreds of ms to seconds down to under 20 ms (Bun 9 ms cached; Zig 63 ms) | L |
| 9 | Cranelift for workspace crates, LLVM for cached deps | Dependencies built once with LLVM stay cached; edited crates use Cranelift in dev | ~20% less codegen, ~5% off clean builds (Rust goal data); more on codegen-heavy edits | S (config; Cranelift gaps are the risk) |
| 10 | Fast linker plus debuginfo policy | Auto-select mold or wild on Linux, ld-prime/lld on macOS; `split-debuginfo=unpacked`, `line-tables-only` for deps | Link 2–3x faster than lld, ~30x faster than GNU ld; less I/O | S |
| 11 | Zig-style in-place linking for dev binaries | Per-function slack plus GOT-indirect calls; patch changed sections in place | Link drops from seconds to tens of ms; removes the serial tail | XL (per format, ELF first; wild-based path plausible) |
| 12 | Test result cache | Key by test binary content ID, filter/args, traced file/env reads; replay output with "(cached)" like Go | Only affected crates' tests rerun; often 2–10x on big suites | M (needs access tracing or declared inputs) |
| 13 | Proc-macro expansion cache | Key by macro crate content ID plus input token hash; deterministic allowlist (serde_derive, thiserror, ...) | 10–35% of frontend time in derive-heavy crates **(estimate)** | L (needs rustc hooks or `-Z cache-proc-macros`) |
| 14 | Explainability: `tinymold explain` and `tinymold verify` | Print key inputs and the first differing input; rebuild and compare (Go's `gocachehash`/`gocacheverify`) | Indirect but decisive: how users trust the cache and maintainers fix key bugs | S |
| 15 | Syscall and allocation budget in CI | Track syscalls, futex counts and allocations per no-op build, as Bun's write-up does | Prevents regressions; Bun's 6x syscall gap was its headline | S |

### Suggested order
1. Do #6, #7, #10, #14 and #1 first: cheap and immediately felt.
2. Then #4, #5, #2 and #12: the cache core.
3. Then #8: the daemon.
4. Leave #3, #11 and #13 for last: research-grade, highest ceiling.

---

## 10. Sources

Inline URLs above are the primary citations. Index: Bun
([install](https://bun.com/blog/behind-the-scenes-of-bun-install),
[bundler](https://bun.com/blog/bun-bundler), [env](https://bun.com/docs/runtime/environment-variables));
[esbuild FAQ](https://esbuild.github.io/faq/); Zig
([0.15.1](https://ziglang.org/download/0.15.1/release-notes.html),
[0.14.0](https://ziglang.org/download/0.14.0/release-notes.html), [devlog](https://ziglang.org/devlog/2025/),
[LLVM/patching](https://kristoff.it/blog/zig-new-relationship-llvm/), [Goodbye
C++](https://ziglang.org/news/goodbye-cpp/), [incremental
internals](https://mlugg.co.uk/posts/incremental-compilation-internals/), [AST layout
commit](https://git.medv.io/zig/commit/4dca99d3f6b732c415d270f0c97def144ed6d3b7.html)); Go ([cmd/go
cache](https://pkg.go.dev/cmd/go#hdr-Build_and_test_caching),
[buildid.go](https://go.dev/src/cmd/go/internal/work/buildid.go), [Go at
Google](https://go.dev/talks/2012/splash.article)); tsgo
([announcement](https://devblogs.microsoft.com/typescript/typescript-native-port/), [Why
Go](https://github.com/microsoft/typescript-go/discussions/411)); Rust ([parallel
rustc](https://blog.rust-lang.org/2023/11/09/parallel-rustc/), [Cranelift
goal](https://rust-lang.github.io/goals/2025h2/production-ready-cranelift.html), [RDR
goal](https://rust-lang.github.io/goals/2025h2/relink-dont-rebuild.html), [PR
#155871](https://github.com/rust-lang/rust/pull/155871), [Fast Builds
2026](https://rust-lang.github.io/goals/2026/roadmap-fast-builds.html),
[wild](https://github.com/davidlattimore/wild), [sccache
Rust](https://github.com/mozilla/sccache/blob/main/docs/Rust.md),
[cache-proc-macros](https://doc.rust-lang.org/unstable-book/compiler-flags/cache-proc-macros.html)).
