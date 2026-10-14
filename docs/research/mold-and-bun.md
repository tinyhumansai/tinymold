# mold and Bun: can tinymold build a better linker?

Research date: 2026-10-15. Scope: a source read of mold 3.0 and of Bun, both
now written in Rust, to decide whether tinymold can build a linker that beats
mold for Rust dev loops by borrowing Bun's engineering. Both trees were read at
pinned commits:

- mold [`c522789`](https://github.com/rui314/mold/tree/c522789)
  (2026-10-11, "Mark the Mach-O build ID as a version-3 UUID"), about 92k lines
  of Rust;
- Bun [`f841e367`](https://github.com/oven-sh/bun/tree/f841e367)
  (2026-10-14), after its port from Zig to Rust.

Line references below point at those commits. The earlier
[`linkers.md`](linkers.md) covers why mold is fast (parallel passes, string
merging, I/O tricks); this report does not repeat it. The decision this
research led to is [ADR 0009](../adr/0009-fork-mold-into-a-stateful-linker.md).

## 0. Answer

A faster *cold* linker than mold is not a realistic goal. The mold paper's
ablation study finds no single dominant optimization: every pass is already
parallel, and a full link runs close to the speed of copying the output
([linkers §0](linkers.md#0-state-of-the-world-oct-2026)). Nothing in the source
read below contradicts that.

A linker that **does less work on the second link** is realistic. mold keeps no
state at all between links, and in a Rust edit loop almost every input of the
next link (std, every dependency rlib) is byte-identical to the previous one.
Bun's codebase is a catalogue of techniques for exactly this: content-addressed
caches with cheap validation, binary formats read without parsing, and
patching an executable instead of rebuilding it.

## 1. mold 3.0: what we would start from

### 1.1 License and crate layout

- MIT, Copyright (c) 2023 Rui Ueyama
  ([`LICENSE`](https://github.com/rui314/mold/blob/c522789/LICENSE)). MIT code
  can live inside tinymold's GPL-3.0 workspace with its notice kept.
- Workspace members: `cli` (the `mold` binary), `common` (`mold-common`),
  `elf` (`mold-elf`), `macho` (`mold-macho`), `tests` (a shell-test runner)
  and 22 `arch/*` crates. Each arch crate is one function that instantiates
  the generic linker for one target, so cargo compiles targets in parallel
  ([`Cargo.toml`](https://github.com/rui314/mold/blob/c522789/Cargo.toml)).
- Internal crates pin each other at `=3.0.0`. `common` and `elf` describe
  themselves as internal crates.
- Dependencies: rayon, memmap2, hashbrown, xxhash (xxh3), blake3 (build-id),
  zstd, zlib-rs, mimalloc (via `mold-mimalloc`), sha1/sha2 (Mach-O code
  signing). `elf/build.rs` compiles two small C files with `cc`.

### 1.2 It is not a library

- `elf/lib.rs` exports only `arch`, `driver` and `elf`. `Context`, `passes`,
  `input_files` and `symbol` are `pub(crate)`
  ([`elf/lib.rs#L3-L30`](https://github.com/rui314/mold/blob/c522789/elf/lib.rs#L3-L30)).
- The entry points take a whole argv:
  [`driver::main`](https://github.com/rui314/mold/blob/c522789/elf/driver.rs#L31)
  and [`driver::link::<E>(Cmdline)`](https://github.com/rui314/mold/blob/c522789/elf/driver.rs#L83).
- The process model assumes one link per process:
  - it forks unless `--no-fork`
    ([`driver.rs#L102-L104`](https://github.com/rui314/mold/blob/c522789/elf/driver.rs#L102-L104));
  - it builds the global rayon pool, which can only be built once
    ([`driver.rs#L113`](https://github.com/rui314/mold/blob/c522789/elf/driver.rs#L113));
  - errors end in `_exit`
    ([`common/error.rs#L124-L140`](https://github.com/rui314/mold/blob/c522789/common/error.rs#L124-L140));
  - inputs are leaked as `'static`
    ([`common/mem.rs#L68`](https://github.com/rui314/mold/blob/c522789/common/mem.rs#L68));
  - mapped files live in process-wide statics
    ([`common/mapped_file.rs#L30-L38`](https://github.com/rui314/mold/blob/c522789/common/mapped_file.rs#L30-L38)).
- Relocation tables are rewritten in place inside the private copy-on-write
  mapping of each input
  ([`common/mapped_file.rs#L152-L161`](https://github.com/rui314/mold/blob/c522789/common/mapped_file.rs#L152-L161),
  [`elf/input_files.rs#L1270-L1280`](https://github.com/rui314/mold/blob/c522789/elf/input_files.rs#L1270-L1280)).
  Parsed input state is therefore not a pure function of the input bytes once
  the link is under way.

Consequence: a one-shot fork (a new binary built from mold's code) is cheap.
A resident or embedded linker needs per-link arenas, `Result` errors instead
of `_exit`, and a pool that is not global.

### 1.3 The pipeline, and what repeats every link

The ELF driver runs serial passes, each parallel inside
([`elf/driver.rs#L83`](https://github.com/rui314/mold/blob/c522789/elf/driver.rs#L83) onward):

1. [`read_input_files`](https://github.com/rui314/mold/blob/c522789/elf/reader.rs#L302):
   a parallel loop over inputs and a task per archive member. Each member is
   mapped or read, its ELF symbols parsed and its globals registered
   ([`new_object_file`](https://github.com/rui314/mold/blob/c522789/elf/reader.rs#L72-L88)).
2. [`resolve_symbols`](https://github.com/rui314/mold/blob/c522789/elf/passes.rs#L652):
   [`gather_symbols`](https://github.com/rui314/mold/blob/c522789/elf/passes.rs#L396),
   resolution, liveness, lazy
   [`parse_input_sections`](https://github.com/rui314/mold/blob/c522789/elf/passes.rs#L473)
   for reachable files, then a second resolution.
3. [`create_merged_sections`](https://github.com/rui314/mold/blob/c522789/elf/passes.rs#L836):
   splits and hashes every mergeable section (strings, constants,
   `.debug_str`) through
   [`convert_mergeable_sections`](https://github.com/rui314/mold/blob/c522789/elf/input_files.rs#L2276).
4. Import/export, GC, ICF, synthetic and output sections,
   [`scan_relocations`](https://github.com/rui314/mold/blob/c522789/elf/passes.rs#L2501),
   [`compute_section_sizes`](https://github.com/rui314/mold/blob/c522789/elf/passes.rs#L2315),
   layout.
5. Copy chunks into the output, applying relocations during the copy.
6. Build-id: BLAKE3 over shards, then a hash of the hashes
   ([`passes.rs#L4118-L4138`](https://github.com/rui314/mold/blob/c522789/elf/passes.rs#L4118-L4138)).

What repeats for unchanged inputs on every link:

- **Every member of every archive is parsed.** mold skips the archive symbol
  table on purpose
  ([`common/archive_file.rs#L136-L140`](https://github.com/rui314/mold/blob/c522789/common/archive_file.rs#L136-L140),
  [BSD `__.SYMDEF`, #L158-L160](https://github.com/rui314/mold/blob/c522789/common/archive_file.rs#L158-L160))
  and opens every member to decide liveness itself. For Rust that is every
  object in std and in every dependency rlib, on every link.
- **Every mergeable section is split and hashed again**, which in debug builds
  is dominated by `.debug_str`.
- **The whole output is written and hashed again.** Output reuse
  ([`common/output_file.rs#L218-L256`](https://github.com/rui314/mold/blob/c522789/common/output_file.rs#L218-L256))
  keeps the inode and page cache but rewrites every byte.

There is no persistent cache, daemon or incremental mode anywhere in the tree.
"preload" refers only to `LD_PRELOAD` for `mold -run`; "incremental" appears
only in a `.gnu.version_r` builder.

### 1.4 Rust awareness

- An rlib is a plain `ar` archive to mold. `lib.rmeta` gets no special case:
  non-ELF members are dropped.
- A `.comment` starting with `rustc ` marks an object as Rust
  ([`elf/input_files.rs#L1760`](https://github.com/rui314/mold/blob/c522789/elf/input_files.rs#L1760)),
  which affects demangling only.
- Debug info: `--separate-debug-file` with `--detach` already moves debug
  writing off the critical path; `--compress-debug-sections` is sharded and
  parallel; `--gdb-index` understands split DWARF skeletons.

### 1.5 Mach-O is real

- `macho/` is about 30k lines: chained fixups, export trie, unwind info, ObjC,
  TAPI `.tbd`, LTO, ICF, dead-strip, ad-hoc code signing.
- The binary acts as Apple's ld64 when invoked as `ld64` or `ld64.*`
  ([`cli/main.rs#L93-L102`](https://github.com/rui314/mold/blob/c522789/cli/main.rs#L93-L102)).
- 668 shell tests in `tests/macho/` against 665 in `tests/elf/`.
- The README still advertises only ELF. This matters to tinymold because its
  benchmark machine is darwin-arm64 and macOS is currently stuck with
  ld-prime (rust-lld is 3x slower on bat, [measured], see
  [`compilers-2026.md`](compilers-2026.md)).
- Mach-O never writes through a shared mapping: a vnode that had one fails
  ad-hoc code-signature checks at exec, so output goes through an anonymous
  buffer and `pwrite` from writer threads
  ([`macho/output_file.rs#L1-L22`](https://github.com/rui314/mold/blob/c522789/macho/output_file.rs#L1-L22)).

### 1.6 Seams for caching

- **Per-input parse.** `new_object_file` depends on the member bytes and a
  few arguments (`--exclude-libs`, `--wrap`, target). Its file-local output
  (symbol names, binding, visibility, section indices, COMDAT groups) can be
  cached by content hash. Global `SymbolId` and `FileId` values must be
  assigned again on load.
- **Merge fragments.** The split points and hashes of a mergeable section
  depend only on its bytes and entry size.
- **Measurement.** `--perf` prints a per-pass tree of wall and CPU time, and
  `--stats` prints counters such as `parsed_objs`
  ([`common/perf.rs#L177`](https://github.com/rui314/mold/blob/c522789/common/perf.rs#L177)).
  Both are off by default and free when off.
- **Dependency file.** `--dependency-file`
  ([`passes.rs#L4346`](https://github.com/rui314/mold/blob/c522789/elf/passes.rs#L4346))
  lists every file read, including those found through `-l` and linker
  scripts, which is the exact input set a link-skip key needs.

### 1.7 Smaller gaps

- `MOLD_JOBS=1` is the only governor: one per-user lock, one link at a time
  ([`common/jobs.rs#L28-L53`](https://github.com/rui314/mold/blob/c522789/common/jobs.rs#L28-L53)).
  There is no N-slot mode.
- Serial pieces (linker-script and LTO ingestion, `set_osec_offsets`,
  synthetic sections, padding fill, Mach-O's signature sequence) are small
  next to parsing and I/O.

## 2. Bun: techniques that transfer

Bun is a JavaScript runtime, package manager and bundler. Its JavaScript
semantics are irrelevant here; its systems engineering is not.

### 2.1 Two-hash cache entries

[`src/jsc/RuntimeTranspilerCache.rs`](https://github.com/oven-sh/bun/blob/f841e367/src/jsc/RuntimeTranspilerCache.rs):

- The filename is a hash of the source bytes alone; no path is involved
  ([`write_cache_filename`, #L581](https://github.com/oven-sh/bun/blob/f841e367/src/jsc/RuntimeTranspilerCache.rs#L581)).
- A fixed metadata header stores a format version
  ([`EXPECTED_VERSION = 34`, #L67](https://github.com/oven-sh/bun/blob/f841e367/src/jsc/RuntimeTranspilerCache.rs#L67)),
  a hash of every option that affects the output (`features_hash`), the input
  length, and offset/length/hash triples for each section. A mismatch on load
  deletes the entry
  ([#L780](https://github.com/oven-sh/bun/blob/f841e367/src/jsc/RuntimeTranspilerCache.rs#L780)).
- Writes are one `pwritev` into a preallocated temp file that is renamed into
  place ([#L368](https://github.com/oven-sh/bun/blob/f841e367/src/jsc/RuntimeTranspilerCache.rs#L368));
  reads are one `pread` into uninitialized buffers.
- Inputs under
  [`MINIMUM_CACHE_SIZE` (4 KiB), #L77](https://github.com/oven-sh/bun/blob/f841e367/src/jsc/RuntimeTranspilerCache.rs#L77)
  are not cached; the threshold was lowered from 50 KiB after measuring that a
  small cached read is still cheaper than redoing the work.

Transfer: name a parse-cache entry by BLAKE3(member bytes) and check
hash(parse-affecting flags, target, linker version, format version) inside it.

### 2.2 Binary formats read by cast

- The npm manifest cache
  ([`src/install/npm.rs#L937-L949`](https://github.com/oven-sh/bun/blob/f841e367/src/install/npm.rs#L937-L949))
  starts with a versioned magic string, then a `#[repr(C)]` header with
  explicit padding, then typed arrays written as length, alignment padding and
  raw bytes. Reads align the cursor and reinterpret each array in place after
  validating enum and bool fields.
- Atomic publish uses `O_TMPFILE` plus `linkat` on Linux, with a rename
  fallback ([#L1109-L1186](https://github.com/oven-sh/bun/blob/f841e367/src/install/npm.rs#L1109-L1186)).
- The binary lockfile stores tables as struct-of-arrays columns with layout
  guard strings, and strings as offset and length into one buffer built by a
  count-then-allocate `StringBuilder`.

Transfer: a cached symbol table is struct-of-arrays (name offset, binding,
type, visibility, section index, value, size) over one string buffer, loaded
with one read and one validation pass.

### 2.3 Patching an executable instead of rebuilding it

`bun build --compile` appends a payload to the Bun executable:

- ELF ([`src/exe_format/elf.rs#L214`](https://github.com/oven-sh/bun/blob/f841e367/src/exe_format/elf.rs#L214)):
  the payload goes after the last `PT_LOAD`, and the writable segment holding
  the `.bun` placeholder section is grown rather than a new segment added. A
  second write replaces the payload in place
  ([`bun_section_is_payload`, #L454](https://github.com/oven-sh/bun/blob/f841e367/src/exe_format/elf.rs#L454)).
- Mach-O ([`src/exe_format/macho.rs#L91`](https://github.com/oven-sh/bun/blob/f841e367/src/exe_format/macho.rs#L91)):
  the `__BUN` segment is grown in place and only `__LINKEDIT`, which nothing
  references by address, is shifted
  ([`Shifter`, #L533](https://github.com/oven-sh/bun/blob/f841e367/src/exe_format/macho.rs#L533)).
  The binary is then signed again ad hoc with the CodeDirectory sized up front
  ([`build_and_sign`, #L518](https://github.com/oven-sh/bun/blob/f841e367/src/exe_format/macho.rs#L518)).
  The build orders segments so `__BUN` is the last content segment.

Transfer: this is the mechanical half of a "prelink the dependencies once,
append the top crate" link. Roc's surgical linker is the other prior art
([`compilers-2026.md`](compilers-2026.md)).

### 2.4 Probe once, remember the answer

- Package installs pick clonefile on macOS and hardlink elsewhere; the first
  `ENOTSUP` or `EXDEV` demotes the method for the rest of the process
  ([`SUPPORTED_METHOD`, `src/install/PackageInstall.rs#L259`](https://github.com/oven-sh/bun/blob/f841e367/src/install/PackageInstall.rs#L259)).
- File copies try `fcopyfile` on macOS and `FICLONE`, then `copy_file_range`,
  then `sendfile`, then read/write on Linux, caching "FICLONE failed" in an
  atomic ([`src/sys/copy_file.rs#L35-L143`](https://github.com/oven-sh/bun/blob/f841e367/src/sys/copy_file.rs#L35-L143)).
- io_uring is a TODO, not a feature
  ([`PackageInstall.rs#L2422`](https://github.com/oven-sh/bun/blob/f841e367/src/install/PackageInstall.rs#L2422)).

### 2.5 Threads, memory and I/O

- The thread pool is a port of kprotty's zap: all state in one atomic word,
  one waking thread at a time, per-worker rings of 256 tasks with stealing,
  intrusive task batches
  ([`src/threading/ThreadPool.rs#L727`, #L1776](https://github.com/oven-sh/bun/blob/f841e367/src/threading/ThreadPool.rs#L1776)).
- Each bundler worker owns a mimalloc heap freed in bulk.
- On macOS file opens go to a separate small pool, because macOS holds an
  `IORWLock` on every open; this gave up to 60% on macOS and regressed Linux,
  so it is platform-gated
  ([`src/bundler/ThreadPool.rs#L38-L43`](https://github.com/oven-sh/bun/blob/f841e367/src/bundler/ThreadPool.rs#L38-L43)).
  Relevant to a Mach-O linker that opens thousands of rlib members.
- Process-lifetime singletons share one `MAP_NORESERVE` region
  ([`src/bun_alloc/lib.rs#L1109`](https://github.com/oven-sh/bun/blob/f841e367/src/bun_alloc/lib.rs#L1109)).

### 2.6 Dirty tracking

The dev server's incremental graph keeps a `stale_files` bitset and dependency
edge lists, and rebuilds only stale files
([`src/runtime/bake/dev_server/incremental_graph.rs#L278`](https://github.com/oven-sh/bun/blob/f841e367/src/runtime/bake/dev_server/incremental_graph.rs#L278)).
`--hot` itself clears every module and relies on the content-hashed transpiler
cache to make the reload cheap: a full redo over a warm cache is often good
enough before true incrementality is needed.

### 2.7 How Bun links itself

- lld everywhere, cross-language ThinLTO, `--icf=safe` with `-faddrsig`,
  `--gc-sections`.
- A symbol ordering file generated from traced first calls
  ([`scripts/build/flags.ts#L1206-L1211`, #L1426](https://github.com/oven-sh/bun/blob/f841e367/scripts/build/flags.ts#L1426)),
  passed as `--symbol-ordering-file` or `-order_file`. A linker tinymold ships
  should accept both.
- `src/linker.lds` is a version script, not a layout script.

### 2.8 Review rules

Bun's review guide asks for each piece of work to be done exactly once and for
syscalls to be counted: attempt the operation and branch on errno rather than
probe first
([`.claude/docs/landing-prs.md#L31`](https://github.com/oven-sh/bun/blob/f841e367/.claude/docs/landing-prs.md#L31)).

## 3. Where a forked linker can beat mold

Ranked by expected impact on Rust dev loops, highest first. None of these
numbers are measured yet; the first phase of the [plan](../plans/linker.md)
measures them.

| # | Improvement | Bun technique | Expected effect | Difficulty |
|---|---|---|---|---|
| 1 | Persistent per-input parse cache, keyed by BLAKE3 of each archive member, stored in the tinymold CAS | Two-hash entries (§2.1), cast-read formats (§2.2) | Parsing of unchanged std and dependency objects disappears from every link | M |
| 2 | Merge-fragment cache: cached split points and hashes for mergeable sections | Same | `.debug_str` re-hashing disappears for unchanged inputs | M |
| 3 | Warm resident linker in the planned `tinymold-server`: dependency state stays in memory between links | Per-worker arenas, zap pool (§2.5) | Link time scales with the changed crate, not the dependency graph | L |
| 4 | One shared copy of dependency state for the N concurrent links of `cargo test`, with an N-slot governor | Same | Most of bat's 2.66 s of `cargo test --no-run` link time [measured, bench/README.md] | L |
| 5 | Prelinked dependency image plus append link | Executable patching (§2.3) | Edit-loop link near 10 ms instead of 60 ms-2 s | XL |
| 6 | Incremental build-id and output: hash and rewrite only changed ranges | Section hashes in cache headers | Avoids rehashing and rewriting large debug outputs | M |
| 7 | Rust awareness: drop `lib.rmeta` early, use the archive index for unchanged rlibs, split and compressed DWARF defaults | Do work once (§2.8) | Small but cheap | S |

On macOS the measured edit-loop link is already short (ripgrep 0.06-0.08 s of
a 0.6-0.7 s loop, [`edit-loop.md`](../plans/edit-loop.md)); the larger wins are
expected on Linux, where every binary carries a full copy of dependency DWARF,
and in `cargo test`, where N binaries link the same dependencies.

## 4. Not worth chasing

- Re-optimizing mold's cold passes. They are already parallel and close to
  copy speed; a fork should not diverge there without a profile that says so.
- io_uring. Bun has not needed it, and mold's bottleneck is not syscall count.
- A new linker written from scratch. mold's 92k lines encode years of
  target-specific correctness (relocations for 20 targets, TLS, IFUNC, ObjC,
  code signing) that a rewrite would have to rediscover.
- Object preloading before the compiler finishes. mold tried it and dropped it
  ([linkers §1.8](linkers.md#18-other-details)); a content-addressed cache
  gives the same benefit without racing the compiler.

## 5. Open questions

- What share of a Rust link is parsing and merging of unchanged inputs? The
  ranking in §3 assumes it is large. Phase L1 of the plan measures it with
  `--perf` on ripgrep and bat before any cache is written.
- Does loading a cached symbol table and re-interning it actually beat
  re-parsing an mmapped ELF symbol table, which is already fast? If not, the
  cache must cover more of the per-file state to pay off.
- How far does the fork drift from upstream? Keeping the source verbatim
  behind package aliases keeps merges cheap only while changes stay at a few
  seams.
