# Linker research for tinymold

Research date: 2026-10-14. Scope: mold, lld, gold, wild, Apple ld-prime/ld64, sold;
link time in Rust builds; incremental linking and hot patching; debuginfo; disk
footprint; cargo configuration; benchmarks. Claims are tagged with sources. Where
I read source code directly (mold 3.0 at `main`, wild at `main`, rustc at `main`)
the file path is given.

## 0. State of the world (Oct 2026)

| Linker | Lang | Platforms | Status |
|---|---|---|---|
| GNU ld (BFD) | C | everything | Default `/usr/bin/ld` on most distros; single-threaded |
| gold | C++ | ELF | Deprecated in binutils 2.44 (Feb 2025), sources split into a separate tarball ([LWN](https://lwn.net/Articles/1007541/), [Phoronix](https://www.phoronix.com/news/GNU-Gold-Linker-Deprecated)) |
| lld | C++ | ELF, COFF, Mach-O, wasm | Rust's default on `x86_64-unknown-linux-gnu` since 1.90 ([blog](https://blog.rust-lang.org/2025/09/01/rust-lld-on-1.90.0-stable/)) |
| mold | **Rust since 3.0** (2026-10-05) | ELF (10+ arches); a `macho/` tree exists in the repo | 2.42.1 was the last C++ release. 3.0 dropped oneTBB, uses rayon + mimalloc ([release notes](https://github.com/rui314/mold/releases/tag/v3.0.0)) |
| wild | Rust | ELF x86-64/AArch64/RISC-V/LoongArch; Mach-O and wasm ports underway | 0.9.0 (May 2026); incremental linking is the stated goal but **not implemented** ([README](https://github.com/wild-linker/wild), [0.9 post](https://davidlattimore.github.io/posts/2026/05/24/wild-update-0.9.0.html)) |
| Apple ld-prime | C++ | Mach-O | Default since Xcode 15. `-ld_classic` is the escape hatch ([forum](https://developer.apple.com/forums/thread/749285)) |
| sold | C++ | Mach-O | Archived 2024-11-25. Its README says to use Xcode 15's linker instead ([repo](https://github.com/bluewhalesystems/sold)) |

Two changes in 2026 matter for tinymold. First, rui314 has softened his stance on incremental linking. The 2.42.1 notes say: "I am not against incremental linking... We are currently exploring designs that can provide those benefits while preserving mold's simplicity and reliability." ([v2.42.1](https://github.com/rui314/mold/releases/tag/v2.42.1)). Second, the mold paper is out: "mold: A Massively Parallel Linker", ASPLOS 2027 ([arXiv 2608.23228](https://arxiv.org/abs/2608.23228)). It reports 2.4–16.1x over lld and up to 112x over GNU ld. Its ablation study finds **no single optimization dominates; the win comes from parallelizing every pass**.

## 1. Why mold is fast

Sources: the original design doc ([docs/design.md @ v2.40.0](https://github.com/rui314/mold/blob/v2.40.0/docs/design.md), marked "written in 2020, contents outdated") and the mold 3.0 Rust source (`common/`, `elf/`).

### 1.1 Overall shape: serial passes, each data-parallel
"From high level, mold just serially executes the linker's internal passes one by one. Each pass is parallelized using parallel for-loops." mold uses no channels, futures or latches. There are three patterns:
1. **Parallel for-each** over files, sections or relocation tables. Shared bits such as `NEEDS_GOT`/`NEEDS_PLT` on a symbol are atomics.
2. **Map-reduce.** Example: build-id is SHA-1 (now BLAKE3 is bundled) over 1 MiB blocks in parallel, then a hash of the hashes, i.e. a Merkle tree of height 2. With 16 cores, a 2 GiB output hashes in 60–70 ms.
3. **Concurrent hash maps** for symbol resolution and string merging.

The pass order in `elf/driver.rs` is: parse inputs → `resolve_symbols` → (LTO) → `remove_unreachable_files` → parse eh_frame/sframe → `create_merged_sections` → version script and import/export → `create_synthetic_sections` → `create_output_sections` → `scan_relocations` → `compute_section_sizes` → `set_osec_offsets` → (`compress_debug_sections`) → open output → `copy` chunks (relocations applied during the copy) → build-id → close.

### 1.2 Fix the layout early, then copy like `cp`
"The most important thing is to fix the layout of an output file as quickly as possible", so that copying can start. mold rejected starting the copy before the layout is known. Its reason: applying relocations *while* copying is "effectively zero-cost due to a very good data locality". Patching them later means re-reading sections that are already out of cache.

### 1.3 Symbol resolution decoupled from archive semantics
Traditional linkers resolve symbols and extract archive members in one loop, with order-dependent semantics, which serializes the work. mold instead:
- **ignores the archive symbol table** and parses every member speculatively;
- resolves all symbols in parallel into a concurrent map;
- marks live objects by graph traversal from the roots;
- **clears the state and redoes resolution** over just the live set (comment in `elf/passes.rs::resolve_symbols`).

The paper names this decoupling, "entangled symbol resolution and archive processing", as the core reason other linkers don't scale.

### 1.4 String merging: a custom lock-free concurrent hash map
`common/concurrent_map.rs` is an open-addressing table, and insertion is lock-free. A thread CASes the key pointer to claim a bucket, writes the value, then publishes the key. A thread that finds the bucket claimed spins until the key appears. Probing is confined to a shard, so shards lay out independently and the output order is deterministic. The table **never resizes**: it aborts if full. It is therefore sized for 2/3 occupancy from a **HyperLogLog estimate** of distinct fragments (`common/hyperloglog.rs`, `elf/chunks/merged.rs`). Alignment is written only when it grows, which avoids cache-line ping-pong on popular strings. Keys are hashed once by the caller, and buckets are padded to avoid false sharing.

Wild does it differently (`libwild/src/string_merging.rs`). It hashes slices into buckets in phase 1, then inserts each bucket into its own map in phase 2, alternating the phases. Debug-heavy links such as clang-debug are "dominated by how quickly we can deduplicate strings" (wild [benchmarks](https://github.com/wild-linker/wild/blob/main/benchmarks/ryzen-9955hx.md)). This is the key hot loop for Rust debug builds, because `.debug_str` is a mergeable section.

### 1.5 I/O: mmap vs read, huge pages, fallocate, output reuse
- **Small inputs are read(2), big ones mmap(2)** (`common/mapped_file.rs`). mmap takes the address-space lock, so tens of thousands of mmaps serialize at a few µs each regardless of thread count. On Chromium, files below the threshold are 64% of inputs by count but 16% by bytes. The read buffers need huge pages (mimalloc does this), or page faults cost more than the mmaps did.
- **`MADV_HUGEPAGE`** on large buffers (`common/mem.rs`).
- **`fallocate` the output on ext4**, which moves block allocation out of the page-fault path. It is skipped on tmpfs, where it single-threadedly zeroes about 5 GiB in 1.8 s (`common/output_file.rs`). Wild's author found the absence of fallocate and hugepages explained most of wild's ext4 deficit vs mold ([wild-vs-mold post](https://davidlattimore.github.io/posts/2026/09/18/benchmarking-wild-vs-mold.html)).
- **Overwrite the existing output in place** if possible. Writing an existing, page-cached file is "much faster than creating a fresh file"; mold measured 300 ms saved on a 2 GiB output. It falls back on ETXTBSY. The output is mapped with **2x address space** so it can grow via ftruncate without remapping.
- **SIGBUS handler** that turns a disk-full error during mmap writes into a readable error.

### 1.6 Exit latency: fork-and-detach, parallel unmapping
Exiting a process that has mapped gigabytes "may take a few hundred milliseconds". mold therefore forks: the child links, signals the parent through a pipe once the output is complete, and the parent exits at once while the child tears down (`common/subprocess.rs`). mold 3.0 also **`MADV_DONTNEED`s input mappings in parallel** before exit, because the kernel otherwise reclaims them on one thread. Wild does the same fork trick (`libwild/src/subprocess.rs`) and also frees big buffers on background rayon tasks ([wild perf tricks](https://davidlattimore.github.io/posts/2025/09/02/rustforge-wild-performance-tricks.html)).

### 1.7 GC, ICF, relocation scanning
- `--gc-sections`: mark-sweep over sections (vertices) and relocations (edges), with **concurrent marking**.
- **ICF** (`elf/icf.rs`) treats folding as a graph-isomorphism problem. It hashes each section's contents and relocations, then iteratively appends the previous round's hashes of callees, so round n covers call chains n−1 deep. It stops when the count of unique hashes stops growing. This is 5x faster than lld on Chromium (5 s → 1 s). ICF typically saves "a few percent" of size, more on monomorphization-heavy Rust code.
- **Relocation scanning** is a parallel loop over relocation tables, which sets atomic flags on symbols to size .got/.plt/.dynsym. Wild instead folds this into a graph traversal in `layout.rs` that also determines which sections are live.

### 1.8 Other details
- Concurrency throttle `MOLD_JOBS=1` (`common/jobs.rs`). Build systems run one linker per core assuming linkers are single-threaded. With a parallel linker this only multiplies peak RSS. **This matters for `cargo test`, which links many binaries concurrently.**
- Since 2.42, mold drops `.L` temporary local symbols by default, making debug output "a few percent smaller".
- "Preloading" (a daemon that pre-parses objects before the compiler finishes) was in the 2020 design but is now explicitly abandoned: "we no longer believe that object preloading is a good idea". inotify watching was rejected too, because a stat() per input file costs under 100 ms even for Chrome.

### 1.9 Wild's distinct tricks (Rust-specific)
Contiguous per-object `SymbolId` ranges allow `split_off_mut` per-object mutable slices with no locks. The `sharded-vec-writer` crate does parallel Vec init. In-place `Vec<SymbolId>` ↔ `Vec<AtomicSymbolId>` conversion reuses the allocation. `reuse_vec` keeps buffers across loop iterations with lifetime-erasure ([source](https://davidlattimore.github.io/posts/2025/09/02/rustforge-wild-performance-tricks.html)). The phases are args → mmap inputs → string merging → symbol DB → resolution → layout (GC and relocation graph) → write ([DESIGN.md](https://github.com/wild-linker/wild/blob/main/DESIGN.md)).

## 2. Benchmarks (with sources)

**mold README, Aug 2026.** All three linkers built 2026-08-28 with default options. Figures are wall-clock medians of 3 runs ([README](https://github.com/rui314/mold)).

| Threadripper 7980X (64c), debug | lld | wild | mold |
|---|---|---|---|
| Chromium 145 (4.51 GiB) | 16.64 s | 3.98 s | 1.65 s |
| Clang 21 (4.19 GiB) | 6.19 | 3.71 | 1.34 |
| ClickHouse 26.1 (5.58 GiB) | 6.62 | 4.40 | 0.98 |
| Blender 5.2 (2.46 GiB) | 4.72 | 1.98 | 0.86 |
| TensorFlow 2.21 (9.55 GiB) | 50.73 | N/A | 3.15 |

On the M1 Ultra (16 performance cores) the gaps shrink a lot. Wild/mold ratios there are 0.8x–2.1x, with wild *faster* on Blender-release (0.20 s vs 0.25 s) and Clang-debug (2.78 s vs 2.96 s). mold's headline is a median 4.9x vs lld and 1.9x vs wild.

**The methodology dispute** ([Lattimore, 2026-09-18](https://davidlattimore.github.io/posts/2026/09/18/benchmarking-wild-vs-mold.html)). mold's benchmark deletes the output between runs, uses ext4 and passes `--no-fork`. On clang-release the wild/mold ratio moves from 1.0x (ext4, delete, no-fork) to **0.6x** (tmpfs, reuse output, fork): 0.11 s vs 0.19 s. Mold may also have used 32 threads against wild's 128 on the Threadripper. The lesson for tinymold is that **filesystem, output reuse, fork and thread count change results by up to 1.7x**, so any benchmark must pin all four.

**Rust-specific numbers**
- ripgrep debug: about half of build time is in the linker. Switching to lld cuts linking about 7x on incremental rebuilds, about 40% end-to-end, and about 20% on from-scratch builds ([Rust blog 2025](https://blog.rust-lang.org/2025/09/01/rust-lld-on-1.90.0-stable/)).
- Lattimore, warm `cargo run` on a 2024 project ([post](https://davidlattimore.github.io/posts/2024/02/04/speeding-up-the-rust-edit-build-run-cycle.html)): GNU ld 20.2 s → lld 1.60 s → strip debuginfo 1.51 s → static non-PIE with musl and mold 1.16 s, about 16x overall. `-Ztime-passes`: link 454 ms of 939 ms, about 48%.
- corrode example: `run_linker` was 0.6–0.76 s of a 3.8 s build ([corrode](https://corrode.dev/blog/tips-for-faster-rust-compile-times/)).
- Rust compiler perf survey 2025 (3,700 responses): incremental rebuilds were the top pain, and about 55% wait more than 10 s on their worst project. Linking is one of the three named bottlenecks because it "is always done from scratch". `line-tables-only` saved 2–30% of cycles even with lld ([survey](https://blog.rust-lang.org/2025/09/10/rust-compiler-performance-survey-2025-results)).
- Integration tests: each `tests/*.rs` file is a separate binary and a separate link. Merging them into one crate was reported at about 50% faster ([corrode](https://corrode.dev/blog/tips-for-faster-rust-compile-times/)).

**Apple.** Xcode 14's ld64 was "twice as fast for many projects", from parallel content copy, parallel LINKEDIT construction, parallel UUID/code-signature hashing and a `string_view` exports trie ([WWDC22 "Link fast"](https://developer.apple.com/videos/play/wwdc2022/110362/)). Xcode 15 replaced it with ld-prime, "written to speed up static linking", but Apple published no figure. The talk's tips: `-no_exported_symbols` saved 2–3 s on an app with about 1M exports, and `-no_deduplicate` (ICF off) is the Debug default.

## 3. What fraction of a Rust build is link time

Link share is highest exactly where developers wait most:
- **Incremental debug rebuild of a binary:** rustc reuses most codegen, but the link is from scratch, so linking is typically **40–50%+** of the edit-run loop (ripgrep ~50%; Lattimore 48%).
- **`cargo test`:** cargo links one binary per lib unit-test target, per `tests/*.rs` file, per example and per doctest. Every binary statically links all of its dependency rlibs *and their debuginfo*. N test files means N full links of the same dependency graph.
- **Clean builds:** linking is a small share (my estimate: ~5–20%, not measured) because codegen of dependencies dominates. The 20% end-to-end lld win on ripgrep is from-scratch, though, so it is not negligible.
- **Doctests:** each doctest was historically compiled and linked separately. Edition 2024 merges doctests into one binary where possible, a link-count fix of the same kind.

## 4. Disk footprint: how linking inflates `target/`

| Mechanism | Evidence |
|---|---|
| Debuginfo is duplicated: once in `.rlib`/`.o`, again in every linked binary that uses that dependency | Cargo's own tests: about 7 GB on Linux vs 2.2 GB on macOS. Test binaries about 6 MB each on Linux vs 1.5 MB on macOS. The gap comes from Linux copying libstd's debuginfo into every binary, while macOS leaves DWARF in the objects and links only a debug map ([cargo#9701](https://github.com/rust-lang/cargo/pull/9701)) |
| Each test binary statically links every dependency | Same as above; scales with (#test targets × dependency size) |
| Uncompressed DWARF | ripgrep debug `target/` 508 MB → 329 MB with zlib debug sections ([rust#116702](https://github.com/rust-lang/rust/pull/116702)). Hello world 4.7 MB → 1.5 MB, arrow-rs 2.1 GB → 1.8 GB with `-Wl,--compress-debug-sections=zstd` ([internals](https://internals.rust-lang.org/t/pre-mcp-set-compressed-debug-sections-zstd-as-the-default-for-linux/20039)) |
| Compression ratio on a large binary | hyperqueue (line-tables-only): zlib 35%, zstd 33%, DWARF GC + zstd 28% of original size. zstd took about 0.1 s vs about 1 s for zlib ([Kobzol 2025](https://kobzol.github.io/rust/2025/09/22/reducing-binary-size-of-rust-programs-with-debuginfo.html)) |
| Linkers keep dead debuginfo | `--gc-sections` drops code but DWARF for it stays as "tombstone" values. DWARF GC (`llvm-dwarfutil`) cut 17% but takes seconds, and an lld attempt reportedly made links 7x slower (Kobzol) |
| split-debuginfo | macOS `unpacked`: `target/` 1.1 GB → 0.8 GB, and a 70% faster incremental build was reported ([corrode](https://corrode.dev/blog/tips-for-faster-rust-compile-times/), [internals](https://internals.rust-lang.org/t/help-test-faster-incremental-debug-macos-builds-on-nightly/14016)). Linux `unpacked` leaves `.dwo` files next to objects, so the linker never copies DWARF. That is one copy instead of N, with no dwp step. `packed` runs `thorin` (dwp), which costs time |
| Stripping test binaries | Yocto: 300+ MB → about 140 MB of tests with `-C strip=debuginfo` ([patch](https://patchwork.yoctoproject.org/project/oe-core/patch/20251027102900.65173-4-peter.tatrai.ext@siemens.com)) |
| Generic duplication | `-Zshare-generics` is **already on by default at opt-level 0/1/s/z** (rustc `Options::share_generics`, `compiler/rustc_session/src/config.rs`). Debug builds reuse upstream monomorphizations instead of re-instantiating them, so less code and DWARF reach the linker. It is off at -O2/-O3 |
| Dynamic dev linking | `prefer-dynamic` / Bevy's `dynamic_linking` build deps as one `.so`, so test binaries share **one copy** of the deps' code and DWARF and links shrink to the small top crate. Bevy claimed about 2x faster builds ([bevy#808](https://github.com/bevyengine/bevy/pull/808)). Caveats: Windows symbol limits, `libstd.so` rpath, and no size figures published |
| ICF | mold/lld `--icf=all` (`safe` is safer for function-pointer equality). "A few percent" for C++; Rust monomorphization yields more identical bodies. Costs link time, so it is a size optimization rather than a dev-loop win |

Takeaway: on Linux the biggest multiplier is **N binaries × full dependency DWARF**. Unpacked split-debuginfo, compressed debug sections and a shared dev dylib each attack a different factor of that product.

## 5. Incremental linking and hot patching

| System | Mechanism | Notes |
|---|---|---|
| **MSVC `/INCREMENTAL`** ([docs](https://learn.microsoft.com/en-us/cpp/build/reference/incremental-link-incrementally)) | `.ilk` database, padding after functions and data, **jump thunks** (ILT) so functions can move | Implied by `/DEBUG`. Full relink if `/OPT:REF`, `/OPT:ICF`, `/ORDER` is set, an .obj is added or removed, options change, or the .ilk/.exe timestamp changes. Output is larger and not reproducible |
| **gold `--incremental`** | Patch the previous output | Falls back to a full link when a function is removed. A null incremental link of Chrome took about 30 s (rui314, design.md). Deprecated |
| **Zig self-hosted linker** | Compiler-integrated. The output is mmapped as a tree of nodes, and a changed function's node resizes in place. If there's no room, neighbours move and dirty flags trigger address and relocation fixups. Exponential slack makes moves rare | 37 ms incremental update for a real app, of which about 1.6 ms is sema+codegen+link and about 30 ms an unnecessary reference-graph walk ([mlugg](https://mlugg.co.uk/posts/incremental-compilation-internals/), [summary](https://www.developersdigest.tech/blog/zig-incremental-compilation-internals-hn-analysis)). Zig 0.17 targets x86_64-linux |
| **wild (design, unimplemented)** ([post](https://davidlattimore.github.io/posts/2024/11/19/designing-wilds-incremental-linking.html)) | Three modes: non-incremental, initial-incremental (reserve slack, write `<out>.incr/` state) and incremental-update. Changes are detected by mtime, but rlib members are compared byte by byte because rustc doesn't set member timestamps. Sections are matched by mangled name, and anonymous `.rodata` by its referrers. State files are mmapped (index, symbol map via odht, resolutions, per-symbol relocation linked lists updated with CAS, eh_frame index). Sections that overflow are moved and their referrers rewritten, otherwise it falls back | Goal: a test result about **10 ms after save**. `--gc-sections` is skipped in incremental mode. Debug info and hot reload are not designed yet. "Incremental, efficient linking with Wild" is on the Rust 2026 Fast Builds roadmap ([roadmap](https://goals.rust-lang.org/2026/roadmap-fast-builds.html)) |
| **Live++** ([site](https://liveplusplus.tech/)) | Recompiles changed TUs and patches the machine code of the running process. Works with MSVC, Clang and LLD on Windows and consoles | Commercial. Internals are not documented publicly; function detours via patched prologues are widely understood |
| **Subsecond / Dioxus 0.7** ([docs.rs](https://docs.rs/subsecond/)) | Calls go through `subsecond::call` and a jump table of latest fn pointers. The CLI recompiles only the tip crate, **links the patch against the running binary's addresses** and ships a new table | Rust-native and works today. Limits: no struct layout changes, thread-locals reset, tip crate only |
| **mold** | Full links "about as fast as copying a file", so incremental was deprioritized. 3.x is "exploring designs" ([v2.42.1](https://github.com/rui314/mold/releases/tag/v2.42.1)) | rui314's original objections: weak symbols and archive extraction cascade non-locally, null-link overhead, non-reproducibility |

Upstream of linking, Rust's "Relink, Don't Rebuild" goal aims to avoid rebuilding downstream crates when only function bodies change, claiming 5–10x on common changes. It is currently marked "will not complete" for 2025h2 ([goal](https://goals.rust-lang.org/2025h2/relink-dont-rebuild.html)). For a body-only change, the *link* then becomes nearly the whole remaining cost.

## 6. Configuring cargo today

```toml
# .cargo/config.toml — Linux, mold (needs clang or GCC >= 12.1)
[target.'cfg(target_os = "linux")']
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=/usr/local/bin/mold"]   # absolute path
```

```toml
# Linux, wild (GCC >= 16.1 can drop the linker line)
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-Clink-arg=--ld-path=wild"]
```

```toml
# Linux x86_64: rust-lld is already the default since 1.90. Opt out with:
rustflags = ["-Clinker-features=-lld"]
# Other Linux targets (nightly): -Zlinker-features=+lld; stable: -Clink-arg=-fuse-ld=lld
```

```toml
# Debuginfo and size knobs for the dev loop
[profile.dev]
debug = "line-tables-only"     # backtraces keep file:line; 2–30% faster cycles
split-debuginfo = "unpacked"   # macOS default; on Linux, DWARF stays in .dwo
[profile.dev.package."*"]
debug = false                  # no DWARF for dependencies at all
```

- Compressed debug: `-Clink-arg=-Wl,--compress-debug-sections=zstd` (mold, lld, wild; mold 2.42 accepts `zstd:N`). Old gdb, binutils and valgrind can't read zstd, so use zlib for compatibility.
- `mold -run cargo build` intercepts `ld*` via LD_PRELOAD on Linux and FreeBSD.
- Verify which linker was used: `readelf -p .comment target/debug/foo`.
- macOS: the system ld is ld-prime and fast enough. `-Clink-arg=-fuse-ld=lld` (ld64.lld) is an option. Do not use zld or sold, which are archived.
- Many concurrent links (`cargo test`): cap them with `MOLD_JOBS=1` (or `-Clink-arg=-Wl,--threads=N`) to avoid out-of-memory.

## 7. Ideas for tinymold (ranked by expected impact / difficulty)

| # | Idea | Expected impact | Difficulty |
|---|---|---|---|
| 1 | **"Fast-dev profile" installer/doctor.** Detect the platform, select mold, wild or lld, set `line-tables-only` for deps, unpacked split-debuginfo and zstd debug compression, and verify with `.comment`. Report link time via `-Ztime-passes` or wrapper timing | 1.5–15x link speedup for anyone still on GNU ld (non-x86_64 Linux, distro rustc); 30–60% less `target/` | Low |
| 2 | **Linker wrapper with telemetry and caching.** A `linker = "tinymold"` shim that hashes the inputs and arguments, and **skips the link entirely when the hash is unchanged**. cargo relinks on mtime churn, e.g. touched build scripts or feature unification. Also logs per-link time and size | Eliminates spurious relinks; gives the data for every other idea | Low–Med |
| 3 | **Test-binary consolidation / "one link for all tests"**. Generate a single harness binary that includes every `tests/*.rs` as a module, or link tests against a shared dylib of the workspace's dependencies | Up to N→1 links for `cargo test`; ~50% faster test builds reported; huge disk savings (cargo's own 7 GB case) | Med |
| 4 | **Dev-only dynamic dependency dylib (generalized `bevy/dynamic_linking`)**. Automatically wrap the whole dependency graph in one `dylib` (`-C prefer-dynamic`) so every test binary and the main binary share one copy | Link only the top crate (tens of ms); one copy of deps' DWARF | Med–High (Windows export limits, rpath, generics/`share-generics` interactions) |
| 5 | **Daemonized linker with warm state.** Keep parsed rlibs, symbol tables and merged-string maps resident between links (the "preloading" idea mold dropped, retried for Rust's case where deps never change). Re-parse only changed `.o`/`.rlib` members | Dependency parsing and string merging gone: link time ∝ changed crate size | High |
| 6 | **Incremental output patching (wild design)**: slack per output section, per-section relocation index, and patching only the sections from changed CGUs. Fall back to a full link on symbol-set changes. Dev builds only, so non-reproducibility is acceptable | Link of 1 changed CGU in ~10 ms vs 0.3–2 s | High |
| 7 | **Jump-table hot patching (Subsecond-style) for `cargo run`/tests.** Link a patch dylib against the running process's symbol addresses | Sub-second edit-to-running-code without a relink | High; correctness limits on layout changes |
| 8 | **DWARF-free main links**: on Linux, force `split-debuginfo=unpacked` plus a lazily built `.dwp` or `--gdb-index` only when a debugger attaches | Removes the largest byte stream (`.debug_*`, often >70% of debug output) from the critical path | Low–Med |
| 9 | **Shared, content-addressed debuginfo store**: dedupe identical `.dwo`/DWARF across test binaries and worktrees (hard links or reflinks). Pairs with `sccache` | Large cuts to `target/` across many binaries and worktrees | Med |
| 10 | **Concurrency governor for `cargo test` links** (MOLD_JOBS-like semaphore plus a per-link `--threads` budget) | Prevents out-of-memory and thrash when cargo spawns 32 parallel multi-threaded links; better tail latency | Low |
| 11 | **Output reuse, fallocate, hugepage and fork tuning**: always link into the existing output file, fork/detach, and keep `target/` on a filesystem where fallocate helps, or on tmpfs where it doesn't | 1.2–1.7x on link wall time (wild/mold dispute shows the swing) | Low |
| 12 | **Benchmark harness** pinning filesystem, output reuse, fork and thread count; Rust-shaped corpora (ripgrep, a Bevy app, a test-heavy workspace) across mold, wild and lld | Avoids fooling ourselves; publishes reproducible numbers | Low |
| 13 | **Opt-in ICF and `--gc-sections` only for the size profile**; never for the dev loop, since MSVC and gold both disable incremental linking under them | Clarity; avoids slow dev links | Low |
| 14 | **Contribute to wild's incremental work** (state files, Rust-specific rlib diffing, debuginfo) rather than writing a new linker | Leverages the Rust roadmap item; highest long-term upside | High; coordination required |

**Suggested sequencing:** 1 → 2 → 12 → 10/11 → 3/8 → 4 → 5/6/7. Items 1–3 deliver most of the user-visible win in weeks. Items 5–7 are where tinymold could beat mold, which is already near `cp` speed for full links: by not doing the full link at all.

## Sources (primary)
- mold README and benchmarks: https://github.com/rui314/mold
- mold design doc (2020): https://github.com/rui314/mold/blob/v2.40.0/docs/design.md
- mold releases 2.40–3.0: https://github.com/rui314/mold/releases
- mold paper: https://arxiv.org/abs/2608.23228
- wild README, DESIGN, FAQ, USAGE, benchmarks: https://github.com/wild-linker/wild
- Wild incremental design: https://davidlattimore.github.io/posts/2024/11/19/designing-wilds-incremental-linking.html
- Wild perf tricks: https://davidlattimore.github.io/posts/2025/09/02/rustforge-wild-performance-tricks.html
- Wild vs mold benchmarks: https://davidlattimore.github.io/posts/2026/09/18/benchmarking-wild-vs-mold.html
- Edit-build-run cycle: https://davidlattimore.github.io/posts/2024/02/04/speeding-up-the-rust-edit-build-run-cycle.html
- rust-lld default: https://blog.rust-lang.org/2025/09/01/rust-lld-on-1.90.0-stable/
- Compiler perf survey: https://blog.rust-lang.org/2025/09/10/rust-compiler-performance-survey-2025-results
- Fast Builds roadmap: https://goals.rust-lang.org/2026/roadmap-fast-builds.html
- Cargo profiles: https://doc.rust-lang.org/cargo/reference/profiles.html
- Debuginfo size: https://kobzol.github.io/rust/2025/09/22/reducing-binary-size-of-rust-programs-with-debuginfo.html
- zstd debug default discussion: https://internals.rust-lang.org/t/pre-mcp-set-compressed-debug-sections-zstd-as-the-default-for-linux/20039
- MSVC incremental: https://learn.microsoft.com/en-us/cpp/build/reference/incremental-link-incrementally
- Zig incremental internals: https://mlugg.co.uk/posts/incremental-compilation-internals/
- Subsecond: https://docs.rs/subsecond/ ; Live++: https://liveplusplus.tech/
- WWDC22 Link fast: https://developer.apple.com/videos/play/wwdc2022/110362/
