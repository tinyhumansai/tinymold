# What tinymold borrows from Zig's build system

Date: 2026-10-14. Scope: a third pass on Zig, this time on the build system
and its cache rather than the compiler. Zig's compiler work (skipping LLVM
in debug builds, the threaded pipeline, in-place binary patching,
data-oriented design, `--watch`) is covered in
[fast-tools §3](fast-tools.md#3-zig) and Bun's Zig fork in
[compilers-2026 §6](compilers-2026.md#6-bun-and-its-zig-fork); this report
does not repeat them.

Zig is worth a third look because it solves tinymold's problem in a small,
readable codebase: a content-checked cache shared by many concurrent
processes, with no daemon, no database and no GC. Most of what follows was
read in the source at Codeberg master
[`ea7667f`](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d)
(14 October 2026) and in the release notes for 0.14.0 to 0.17.0. Source
links pin that commit, so line numbers stay valid.

Confidence labels follow [compilers-2026](compilers-2026.md): **[P]** = read
in the primary source during this research; **[S]** = secondary source only;
**[M]** = from memory, not re-fetched. Claims re-fetched and re-read while
writing this report (rather than taken from the research notes) are marked
**[P, checked]**. Nothing here was reproduced by tinymold.

## 1. Summary

- **One cache design, used for everything.** `std.Build.Cache` keys the
  compiler, C objects, runtime libraries, run steps and (from 0.17) the build
  script's own configuration. A manifest per key lists every input file with
  its stat data and digest.
- **Concurrency comes from file locks, not a server.** A hit holds a shared
  lock on the manifest; a fill holds an exclusive one; a process that misses
  under a shared lock re-checks after upgrading. Two `zig build` runs on one
  tree compile each unit once.
- **Two cache modes.** *Whole* mode keys on all inputs and is immutable.
  *Incremental* mode keys only on options, so one output directory is patched
  in place edit after edit. This answers tinymold's open question on
  incremental versus caching (§2.7).
- **Racy timestamps are measured, not assumed.** Zig writes a probe file to
  learn "now" in the filesystem's own clock. Comparing against it found a bug
  in tinymold (§7).
- **What Zig gets wrong is GC.** Caches of 30 GB and 140 GB were reported; the
  GC issue has been open since 2023 (§2.6). tinymold already pays the price
  Zig avoided: one metadata write per hit.

## 2. `std.Build.Cache`

### 2.1 Two-phase hashing and path prefixes

- The hasher is SipHash128(1,3) with a fixed key, producing 16-byte digests
  ([Cache.zig#L127-L130](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/std/Build/Cache.zig#L127-L130))
  [P, checked]. The key changes when the manifest format breaks.
- Up to five path prefixes (cwd, `zig_lib`, local cache, global cache, build
  root) replace absolute paths by an index
  ([Compilation.zig#L1959-L1979](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/src/Compilation.zig#L1959-L1979))
  [P, checked]. Paths outside every prefix are stored absolute.
- **Phase 1** hashes options and input *paths* into the manifest name,
  `h/<hex>`. **Phase 2** reseeds with that digest and folds in every file's
  content digest, including files discovered during the compile, to name the
  output directory `o/<hex>`
  ([Cache.zig#L900-L1010](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/std/Build/Cache.zig#L900-L1010))
  [P, checked].
- Discovered inputs come from depfiles: clang runs with `-MD -MV -MF`, and
  `addDiscoveredPath` / `addDiscoveredDepFile` (0.17; `addFilePost` /
  `addDepFilePost` before) add them after the compile [P].
- On a miss, outputs go to `tmp/<random>/` and are renamed to `o/<digest>/`
  [P].

This is tinymold's pre-key and full key
([cache-key spec](../specs/cache-key.md#two-phase-lookup)) arrived at
independently. The one difference: Zig's phase 1 hashes input *paths* only,
while tinymold's pre-key already folds in the content of `--extern`
artifacts. §8.3 returns to this.

### 2.2 The manifest format

- Up to 0.16 a manifest is text, one line per input:
  `size inode mtime_ns hexdigest prefix path` [P].
- 0.17 switched to binary, 8-byte-aligned records `{size, inode, mtime,
  digest[16], flags{is_directory, metadata_only, prefix: u6}, path}` [P].
  The 0.17 notes report ~25% smaller files, hits 5-10% faster
  ([#36822](https://ziglang.org/download/0.17.0/release-notes.html#Cache-System-Reworked)),
  a cache that "can explain why a miss happened", and a `zig cache-cat`
  subcommand that dumps manifests [P, checked].
- Two new flags carry ideas tinymold lacks: **directory inputs** and
  **metadata-only inputs** (depend on a file existing or its mtime, not its
  bytes). Both exist for build scripts (§3.3).

### 2.3 The stat check and racy timestamps

- A hit stats each input (size, mtime, inode). Only a mismatch triggers a
  rehash; if the content digest still matches, the hit stands and the stored
  stat data is refreshed [P].
- **Racy mtimes.** `isProblematicTimestamp`
  ([Cache.zig#L1644-L1685](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/std/Build/Cache.zig#L1644-L1685))
  [P, checked] creates a file named `timestamp` in `h/` and reads back its
  mtime: "now", in the filesystem's own clock and granularity. Any input
  whose mtime is at or after that is recorded with mtime and inode 0, so the
  next check must rehash it. The probe runs at most once per manifest check
  and its result is shared across threads behind a mutex.
- Comparing against a probe rather than the wall clock is right on
  filesystems with coarse ticks (HFS+ and ext3: 1 s; FAT: 2 s) and on
  machines whose clock disagrees with a network filesystem's.

### 2.4 Locking: shared hits, exclusive fills

From
[Cache.zig#L915-L1012](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/std/Build/Cache.zig#L915-L1012)
and
[#L2013-L2062](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/std/Build/Cache.zig#L2013-L2062)
[P, checked]:

1. Open `h/<digest>` with an exclusive, **non-blocking** `flock`.
2. If that would block, someone else holds it: open with a **blocking
   shared** lock instead. That waits for any filler to finish.
3. Check. On a hit, done. On a miss under a shared lock, **unlock**, take
   the exclusive lock, and **check again**, since another process may have
   filled the entry in the gap. The release is deliberate: two shared
   holders upgrading at once would otherwise deadlock.
4. After a hit, or after writing the manifest, downgrade to shared and keep
   it for as long as the outputs are in use.

A macOS quirk is handled too: two processes racing `openat(O_CREAT)` can get
`ENOENT`, so Zig retries with `O_EXCL` to tell a race from a deleted
directory (#L937-L966).

The effect is single-flight without a daemon: N processes that need the same
unit block on one lock while the first compiles it, then all hit.

### 2.5 Global and local caches

- A project has a local `.zig-cache/`; the global cache is
  `$ZIG_GLOBAL_CACHE_DIR`, the XDG cache dir or `~/.cache/zig`
  ([zig.zig#L1700-L1730](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/std/zig.zig#L1700-L1730))
  [P]. Subdirectories: `h/` manifests, `o/` outputs, `tmp/`, `z/` ZIR,
  `b/` builtin files, `p/` packages.
- Runtime libraries (compiler_rt, libc, libunwind) always go to the global
  cache in whole mode, built lazily once per (Zig version, target, options)
  [P]. This is the "registry deps once per machine" split tinymold aims for.
- Installing to `zig-out/` **copies**, through `Io.Dir.updateFile`, which
  skips the copy when size, mtime and permissions match and otherwise copies
  atomically, preserving mtime [P]. Zig never hands out links into its cache.

### 2.6 No garbage collection

- [ziglang/zig#15358](https://github.com/ziglang/zig/issues/15358) (2023)
  collects reports of a 30 GB cache holding ~750 builds of one executable and
  a 140 GB cache for one project [P, checked]. It migrated to
  [Codeberg #30193](https://codeberg.org/ziglang/zig/issues/30193), opened by
  Andrew Kelley on 2025-12-14 and **still open** [P, checked].
- The plan is a `zig gc` with LRU caps (defaults: global 1 GiB, local 2 GiB,
  packages 4 GiB) that lists `h/`, tries a non-blocking lock on each manifest
  and deletes `o/` then `h/` only when the lock is free [P, checked]. Kelley:
  the cache is "designed explicitly so that garbage collection could happen
  in one process simultaneously while the cache is being used by another"
  [P, checked].
- Named problems: hits write nothing, so there is no recency signal; `tmp/`
  cannot be reaped safely because a live fill is indistinguishable from a
  dead one [P].
- A user who deleted part of `o/` by hand broke their build: a manifest that
  hits but whose output is missing is not treated as a miss [P].

### 2.7 Two cache modes: whole and incremental

`CacheMode`
([Compilation.zig#L1347-L1379](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/src/Compilation.zig#L1347-L1379))
[P, checked]:

- **whole**: every input is in the key, the output is built in `tmp/` and
  moved to `o/<hash>/`, and it is immutable afterwards.
- **incremental**: the key covers options and target but **not source
  files**, "so that we can find the same binary and incrementally update it
  even if there are modified source files". One `o/<hash>/` is patched in
  place, edit after edit, and holds the incremental metadata.

Zig uses both at once: incremental for the module being edited, whole for
compiler_rt, libc and everything else. That is the answer to tinymold's open
question #1 ([README §6](README.md#6-open-questions-for-planning)): do not
choose between incremental and caching per project, choose per unit. Units
nobody edits are whole-mode, content-keyed and shareable; the crate being
edited keeps one incremental directory keyed by its configuration and is
never shared (§8.4).

## 3. The build runner

### 3.1 `--watch` and the watch set

- `--watch` and `--debounce <ms>` arrived in 0.14
  ([notes](https://ziglang.org/download/0.14.0/release-notes.html#File-System-Watching))
  [P, checked]. The runner stays alive and watches the **smallest set of
  directories** covering every input, using "the build system's perfect
  knowledge of all file system inputs". Default debounce: 50 ms, which
  absorbs editors (vim) that delete then write.
- Backends (`lib/std/Build/Watch.zig` to 0.16,
  `lib/compiler/Maker/Watch.zig` in 0.17) [P]: Linux uses fanotify with
  directory handles from `name_to_handle_at`, one fd per mount (kernel 5.17+);
  BSDs use kqueue; Windows `ReadDirectoryChangesW`; macOS FSEvents, loaded
  with `dlopen` from CoreServices
  ([0.15 notes](https://ziglang.org/download/0.15.1/release-notes.html#macOS-File-System-Watching)).

### 3.2 The compiler server protocol

- [`Server.zig`](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/std/zig/Server.zig)
  [P]: frames are `{tag: u32, len: u32}` plus payload. The client sends
  `update`, `run`, `hot_update`, `exit`; the server answers with
  `error_bundle`, `emit_digest` (a cache-hit flag plus the `o/` digest),
  `file_system_inputs` (NUL-separated, prefix-indexed paths, which become the
  watch set) and `time_report`.
- The compiler stays alive across updates with `--listen=-`. Its incremental
  state lives only in memory, so `-fincremental` still needs `--watch`
  [P]. Incremental without `--watch` is planned, not shipped.
- Status: experimental in 0.14 (14 s → 63 ms on a 500 kLOC codebase, no
  binary emitted) [P, checked]; supported with the LLVM backend in 0.16 but
  "disabled by default" with "known bugs, including some miscompilations"
  [P, checked]; the 0.17 notes say "most projects targeting x86_64-linux" can now use it [P, checked].

The lesson for tinymold: the server reports *what it read* and *which digest
it produced*, and the build runner derives the watch set from that. tinymold
already has the first half in its pre-key records.

### 3.3 Caching the build script itself (0.17)

- 0.17 splits `zig build` into a **configurer** (`build.zig` compiled and
  run, its binary configuration output cached) and a **maker** built once per
  Zig version ([devlog 2026](https://ziglang.org/devlog/2026/), Codeberg
  PR #35428). `zig build -h` went from 150 ms to 14.3 ms [P, checked].
- Configure logic the cache cannot track **poisons** the cache. Tracked
  alternatives are `dependOnFileContents`, `dependOnFileMetadata`,
  `dependOnDirectoryContents` and `dependOnDirectoryMetadata`; a CLI switch
  `--cache-poison=pure|poisoned|disallowed|ignored` picks the policy
  ([0.17 notes](https://ziglang.org/download/0.17.0/release-notes.html#Introduce-the-Concept-of-Configure-Cache-Poisoning))
  [P, checked]. `pure` is the default: avoid false hits.

This is cargo's build-script problem with a vocabulary attached:
`rerun-if-changed` on a directory, a script that reads the environment
without declaring it, a script that only cares whether a file exists.

### 3.4 Run steps and memory budgets

- Run steps are cached like compiles unless they declare side effects
  ([Run.zig#L555-L648](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/compiler/Maker/Step/Run.zig#L555-L648))
  [P, checked].
- **`max_rss` is real.** A step declares an upper bound on its peak memory.
  The runner `ulimit`s the child to it and fails the step if the measured
  peak exceeds it; it also schedules steps so the sum of running steps'
  `max_rss` stays under `--maxrss`, which defaults to total system memory.
  Blocked steps wait in `memory_blocked_steps` until memory is released
  ([Step.zig#L25-L33](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/std/Build/Step.zig#L25-L33),
  [Maker.zig](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/compiler/Maker.zig))
  [P, checked]. The bound is declared by hand, not learned.
- `--time-report` and `--webui` show per-step timings (0.15 notes) [P].

## 4. Linking

- LLD is used only with the LLVM backend, LTO, or pure C
  ([Config.zig#L393-L475](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/src/Compilation/Config.zig#L393-L475))
  [P]. Otherwise Zig links itself.
- The new ELF linker is built on
  [`MappedFile.zig`](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/src/link/MappedFile.zig):
  the output is an mmapped tree of nodes with 25% growth slack. A node that
  outgrows its slack moves its siblings and marks them dirty; fixups run at
  idle or flush. On Linux it grows the file with
  `fallocate(FALLOC_FL_INSERT_RANGE)`, moves bytes with `copy_file_range`
  and frees with `FALLOC_FL_PUNCH_HOLE` [P, checked]. The old linker padded
  atoms by a third (`padToIdeal`).
- Numbers ([0.16 notes](https://ziglang.org/download/0.16.0/release-notes.html#New-ELF-Linker))
  [P, checked]: building the Zig compiler, then two one-line edits, takes
  14 s / 194 ms / 191 ms with the old linker, 14 s / 65 ms / 64 ms with the
  new one, and 14 s / 62 ms / 62 ms with no linking at all. In
  [the devlog](https://ziglang.org/devlog/2026/) the compiler builds in 36 s
  cold and 228-288 ms per update with the LLVM backend [P, checked].
- The linker runs on one thread by design.

Incremental output patching is already on tinymold's list as a contribution
to wild ([linkers §7](linkers.md#7-ideas-for-tinymold-ranked-by-expected-impact--difficulty));
Zig's numbers show the ceiling: once patched in place, linking costs ~3 ms.

## 5. Compiler internals that touch the cache

- **The ZIR cache is keyed by path, not content.** Files in `z/` are named by
  SipHash of (path, Zig version, backend) and validated by stat alone (inode,
  size, mtime in `Zir.Header`); the arrays are written with one `writeVec`,
  with no serialization step; files are shared/exclusive locked [P]. This
  corrects [fast-tools §3.4](fast-tools.md#34-data-oriented-design-dod),
  which says ZIR is "keyed by content hash".
- The InternPool is sharded with per-thread locals and encodes the thread id
  in its indices
  ([InternPool.zig](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/src/InternPool.zig))
  [P]. AstGen runs one task per file, and tasks enqueue imports.
- The x86_64 backend and threaded codegen are in
  [fast-tools §3.1-3.2](fast-tools.md#31-skip-llvm-for-debug-builds).

## 6. Packages and `zig cc`

- **Package hash**
  ([Fetch.zig#L1712](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/lib/compiler/Maker/Fetch.zig#L1712))
  [P, checked]: each file is hashed as SHA-256(normalized path ‖ `0x00 0x00`
  ‖ contents) in parallel; the digests are sorted by path and hashed again.
  The result does not depend on thread count. Files outside `paths` are
  deleted first, and the executable bit is hard-coded false
  ([#17463](https://github.com/ziglang/zig/issues/17463)).
- Since 0.14 a package is named `name-version-<base64url of 33 bytes>` (u32
  id, u32 size, 200 bits of SHA-256) [P]. Since 0.16 packages unpack into a
  project-local `zig-pkg/`, with the tarball kept as global `p/<hash>.tar.gz`
  [P].
- **`zig cc`** generates glibc stub libraries from bundled ABI lists, cached
  by (Zig version, arch, ABI, glibc version)
  ([glibc.zig#L668-L1263](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/src/libs/glibc.zig#L668-L1263))
  [P]; musl, mingw, libunwind, libc++ and compiler_rt are built from bundled
  sources on demand into the global cache.
  [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) uses `zig
  cc` as the linker for cargo; `--target aarch64-unknown-linux-gnu.2.17`
  picks the glibc version [P].

## 7. What tinymold already does the same way

These are convergent designs, not borrowed ones; Zig is a second witness that
they are right.

| Zig | tinymold |
|---|---|
| Path prefixes | Path roots (`${WS}`, `${CARGO_HOME}`, `${OUT}`) in `tinymold-key` |
| Phase-1 manifest, phase-2 output digest | Pre-key record, then full key ([cache-key](../specs/cache-key.md#two-phase-lookup)) |
| Discovered inputs from depfiles | Inputs from rustc's dep-info |
| `tmp/` then rename | Objects and records written to a temp file and renamed |
| Install copies, never links | Restore by reflink or copy, never hardlink |
| Stat check with a racy rule | Stat cache and per-input identities with git's racily-clean rule |

**A bug found by the comparison.** tinymold's racy rule compares file mtimes
to the wall clock: `SystemTime::now()` in `write_entry`
(`crates/tinymold-wrap/src/stat/mod.rs`), in compile ingest
(`crates/tinymold-wrap/src/compile/mod.rs`) and in the freshness record
(`crates/tinymold/src/fresh/mod.rs`). On a filesystem with a 1 s or 2 s tick
the recorded "now" carries nanoseconds the file's mtime cannot, so a file
written later in the same tick can look older than the record, and a change
can be missed. A skewed clock on a network filesystem has the same effect.
Zig avoids this with its probe file (§2.3). A probe does not transfer
directly: tinymold's store usually sits on a different filesystem from the
inputs, so a probe in the store measures the wrong clock. The fix in progress
(branch `stat-racy-tick`) instead infers each file's tick from the trailing
zeros of its mtime and treats the whole tick as racy.

## 8. Ideas, ranked for tinymold

Impact and difficulty use the [README](README.md#3-idea-catalogue) scale.

| # | Idea | Impact | Diff. | Phase |
|---|---|---|---|---|
| 1 | [Lock-based single-flight without the daemon](#81-lock-based-single-flight) | H for agent bursts | S-M | 3 (next) |
| 2 | [The lock as GC liveness](#82-the-lock-as-gc-liveness) | Correctness | S after 1 | 6 (GC) |
| 3 | [Inline stat data for externs in the pre-key record](#83-inline-stat-data-for-externs) | L-M | M | 3, measure first |
| 4 | [Two cache modes: whole and incremental](#84-two-cache-modes) | M-H | M | 5 |
| 5 | [Watch set from discovered inputs](#85-watch-set-from-discovered-inputs) | M | M | 6 |
| 6 | [Build-script cache with poisoning](#86-build-script-cache-with-poisoning) | M-H | M | 3 |
| 7 | [Hermetic `zig cc` linker option](#87-hermetic-zig-cc-linker) | M | M | 4 |
| 8 | [Miss explanation in the record, `tinymold cache cat`](#88-miss-explanation-and-cache-cat) | Enabler | S | 3 |
| 9 | [Edit-sequence correctness corpus](#89-edit-sequence-correctness-corpus) | Correctness | S-M | 3 |

### 8.1 Lock-based single-flight

Take Zig's protocol (§2.4) for pre-key records: try an exclusive
non-blocking `flock` on `prekeys/<pre>`, fall back to a blocking shared lock,
and on a miss unlock, take the exclusive lock and re-check. Eight worktrees
building one commit then compile each unit once in one-shot mode. Today the
wrapper has no cross-process lock, and single-flight waits for the Phase 6
server ([roadmap](../plans/roadmap.md#phase-6-hot-server-and-remote)).

Caveat: a wrapper blocked on a lock still holds one of cargo's job slots, so
a burst of waiters can idle the machine. That is still better than N
identical compiles, but a waiter should not hold the slot for longer than the
filler's expected compile time. This is the next piece of work.

### 8.2 The lock as GC liveness

With 8.1 in place, `tinymold gc` takes a non-blocking exclusive lock on each
record before deleting it and skips any record that is held, which is Zig's
GC plan. A build that is restoring or filling a unit can then never lose it
mid-flight. Small once 8.1 exists.

### 8.3 Inline stat data for externs

The pre-key record already stores each input's identity (device, inode, size,
mtime) and digest, and a lookup reuses the digest when the identity matches
(`crates/tinymold-wrap/src/record/mod.rs`). What remains outside it is
everything hashed *before* the pre-key can be named: each `--extern`
artifact, each search-path listing, and the rustc and linker identities go
through the separate `stat/` cache, one small file per path. Folding those
into the record would need Zig's phase-1 shape, where the pre-key covers
extern *paths* and the record holds their identities and digests. Measure the
`stat/` reads per wrapper call first; on a warm page cache they may already
be negligible.

### 8.4 Two cache modes

Whole mode, as today, for every unit nobody is editing: content-keyed,
immutable, shareable. Incremental mode for the workspace crate being edited:
keyed by configuration only, one directory under the store patched in place
by rustc's own incremental compilation, never shared and never uploaded. This
replaces the choice in open question #1 with a per-unit rule and fits the
existing per-key incremental-dir idea
([README §3.3](README.md#33-hot-cache-server)).

### 8.5 Watch set from discovered inputs

Pre-key records already list every file each unit read. The Phase 6 dirty set
can watch the directories that cover them (not each file), debounce at 50 ms,
and use fanotify per mount on Linux and FSEvents on macOS, as Zig does
(§3.1).

### 8.6 Build-script cache with poisoning

Phase 3's remaining build-script caching should borrow Zig's vocabulary:
directory inputs, metadata-only inputs, and a poisoning policy. A script
whose `rerun-if-*` lines are complete is *pure* and cached; one that emits
none, or reads undeclared environment, is *poisoned* and always runs. A
`--cache-poison`-style switch lets a user pick strictness.

### 8.7 Hermetic `zig cc` linker

Offer `zig cc` (as cargo-zigbuild does) as a linker choice. The linker's
identity then reduces to (Zig version, target, glibc version), the same on
every machine, so link outputs become shareable through the remote tier
instead of differing by host toolchain.

### 8.8 Miss explanation and `cache cat`

Zig 0.17 made the manifest format carry enough to explain a miss, and ships
`zig cache-cat`. tinymold's records are text already; a `tinymold cache cat`
that prints a record with digests resolved, plus a miss explainer that diffs
the nearest recorded input set against the current one, is small and builds
trust ([principle 7](README.md#2-cross-cutting-principles)).

### 8.9 Edit-sequence correctness corpus

Zig's `test/incremental/` holds scripted edit sequences (`#write`, `#update`,
`#check stdout`) run by `zig build test-incremental` across backends and
cache modes
([test/incremental](https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/test/incremental))
[P, checked]. tinymold needs the same shape: apply an edit, expect a hit or a
miss per unit, and expect the outputs to equal a clean build. (The research
notes named a `tools/incr-check.zig` driver; it is not at this commit, where
the cases are driven from `build.zig` and `test/tests.zig`.)

## 9. Parallelism

- **P1. A memory-aware link governor.** Zig admits steps by declared
  `max_rss` against a budget (§3.4). Phase 4's link governor
  ([roadmap](../plans/roadmap.md#phase-4-test-builds-and-links)) should admit
  links by estimated RSS (input sizes, or the last measured peak for that
  binary) rather than a fixed count. Zig's bound is hand-declared; tinymold
  can learn it.
- **P2. Parallel restore on a hit.** Ingest already stores outputs in
  parallel (`crates/tinymold-wrap/src/compile/mod.rs`); restore was serial.
  In progress on branch `parallel-restore`.
- **P3. Order-independent parallel hashing.** Use the package-hash shape
  (§6): hash files in parallel, sort by path, hash the digests. It fits the
  freshness record, directory inputs and pre-key hashing on stat misses. Test
  that 1 and N workers give the same digest.
- **P4. Parallelism must not change bytes.** Zig refused Bun's parallel Sema
  because it was non-deterministic
  ([compilers-2026 §6](compilers-2026.md#6-bun-and-its-zig-fork)).
  `-Zthreads` is not in tinymold's key
  ([cache-key](../specs/cache-key.md#not-in-the-key)), so verify that
  `-Zthreads=1` and `-Zthreads=8` give identical bytes before relying on
  that. `-C codegen-units` does change output, so per-crate sharding applies
  only to units nobody else shares.
- **P5. Overlap tinymold's work with rustc.** Hash outputs as they appear,
  and release cargo once the `.rmeta` is placed if pipelining allows. A spike.
- **P6. A sharded daemon index**, InternPool-style, only if profiling shows
  contention.

## 10. What not to copy

- **No GC.** Zig's 30-140 GB caches are the result, and its hits write
  nothing, so it has no recency to evict by. tinymold's
  [GC spec](../specs/gc.md#last-use) pays one metadata write per hit for
  exactly that reason; keep it.
- **A missing artifact that is not a miss.** In tinymold a failed restore
  already falls back to a miss: `serve` in
  `crates/tinymold-wrap/src/compile/mod.rs` only returns a hit when the
  restore succeeds. Keep that tested.
- **SipHash128 and path-plus-stat keys.** Fine for one machine's private
  cache, not for a shared or remote one. BLAKE3 stays
  ([ADR 0002](../adr/0002-blake3-for-content-digests.md)).
- **State that lives only in memory.** Zig's incrementality still needs
  `--watch` because of it; tinymold's daemon state must serialize from day
  one ([principle 6](README.md#2-cross-cutting-principles)).

## 11. Sources

Source code at Codeberg commit `ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d`
(prefix
`https://codeberg.org/ziglang/zig/src/commit/ea7667f5a5b44d548c398b2f40ffa239b1a1fb6d/`):
`lib/std/Build/Cache.zig`, `lib/std/Build/Step.zig`, `lib/std/zig.zig`,
`lib/std/zig/Server.zig`, `lib/compiler/Maker.zig`,
`lib/compiler/Maker/Fetch.zig`, `lib/compiler/Maker/Step/Run.zig`,
`lib/compiler/Maker/Watch.zig`, `src/Compilation.zig`,
`src/Compilation/Config.zig`, `src/InternPool.zig`, `src/link/MappedFile.zig`,
`src/libs/glibc.zig`, `test/incremental/`.

Release notes:
[0.14.0](https://ziglang.org/download/0.14.0/release-notes.html),
[0.15.1](https://ziglang.org/download/0.15.1/release-notes.html),
[0.16.0](https://ziglang.org/download/0.16.0/release-notes.html),
[0.17.0](https://ziglang.org/download/0.17.0/release-notes.html).
Devlog: [2026](https://ziglang.org/devlog/2026/) (configurer/maker split,
2026-05-26; incremental Zig compiler, 2026-05-30),
[2025](https://ziglang.org/devlog/2025/) (threaded codegen, 2025-06-14).
Issues: [ziglang/zig#15358](https://github.com/ziglang/zig/issues/15358),
[Codeberg #30193](https://codeberg.org/ziglang/zig/issues/30193),
[ziglang/zig#17463](https://github.com/ziglang/zig/issues/17463).
Tools: [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild).
