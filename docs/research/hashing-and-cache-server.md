# tinymold research: hashing, the hot cache server, and storage footprint

Scope: hashing algorithms and where to apply them, how existing build systems
avoid hashing, persistent build daemons and remote caches, cache-key design for
`rustc`, the storage footprint of `target/`, and whether rustc's in-memory state
could be kept warm. Linkers, rustc internals in depth, and VFS are covered by
sibling reports.

Labels used below: **[measured]** means measured on this machine (Apple M5 Pro,
macOS 26, APFS, Oct 2026). **[vendor]** means a project's own benchmark.
**[memory]** means from background knowledge and not re-verified in this pass,
so check it before relying on it.

---

## 1. Hashing

### 1.1 Throughput landscape

| Hash | Class | Throughput (large inputs) | Source |
|---|---|---|---|
| XXH3-64 / XXH128 | non-crypto, 64/128-bit | 31.5 / 29.6 GB/s single-thread (i7-9700K, SSE2); RAM read 28 GB/s | [xxHash README](https://chromium.googlesource.com/external/github.com/Cyan4973/xxHash/+show/a8d6ff0e139491de5bab6a08a117266d446f083a/README.md) |
| rapidhash | non-crypto, 64-bit | M1 Pro 47, M3 Pro 57, M4 71, Ryzen 9700X 68 GB/s (xxh3: 37 / 43 / 49 / 78) | [rapidhash](https://github.com/Nicoshev/rapidhash) [vendor] |
| gxhash | non-crypto, AES-NI/NEON-AES | "fastest of its class for all sizes"; no fallback on CPUs without AES; output stable only within a major version | [gxhash](https://github.com/ogxd/gxhash) [vendor] |
| foldhash | in-memory tables | best average rank vs fxhash/ahash/SipHash-1-3 on M2 and Xeon 8481C; *not* stable across versions; minimal HashDoS resistance | [foldhash](https://github.com/orlp/foldhash) |
| BLAKE3 | cryptographic, 256-bit, Merkle tree | peak single-thread ≈12× SHA-256, 4× BLAKE2b on Cascade Lake-SP; ~8× SHA-256 on 1 MiB in Bazel's Java port (i5-8520U); b3sum multithreaded ≈"order of magnitude" faster than sha256sum | [BLAKE3 paper](https://cgi.cse.unsw.edu.au/~cs4601/refs/papers/blake3.pdf), [BLAKE3 repo](https://github.com/BLAKE3-team/BLAKE3), [Bazel commit](https://bazel.googlesource.com/bazel/+/5e63f2d44a4ed50874595d535fd83e6c8f67a3e0%5E%21) |
| SHA-256 (ARMv8 SHA ext.) | cryptographic | **~1.9 GB/s wall, ~2.6 GB/s CPU** single-thread on a 665 MB rlib (`openssl dgst -sha256`) | [measured] |
| SHA-256 vs BLAKE3, small inputs | — | Below ~4 KB, best SHA-256 (SHA-NI / batched AVX-512) can beat BLAKE3 on x86 | [Solana forum](https://forum.solana.com/t/blake3-slower-than-sha-256-for-small-inputs/829) |

Notes:

- BLAKE3 only reaches its headline numbers on large inputs, where it can
  run SIMD lanes and threads across the chunk tree. The Rust crate's `rayon`
  feature exposes `Hasher::update_rayon`, and its `mmap` + `rayon` features
  expose `update_mmap_rayon` [memory: API name; see docs.rs/blake3]. Use this
  for the few big artifacts: a 600 MB rlib or a test binary.
- On Apple Silicon, Bazel's Java port of BLAKE3 measured *slower* than
  SHA-256 on an M3 Max (≈0.75×), because Java can't use NEON and the CPU has
  SHA-2 instructions. The native Rust crate has a NEON path. Benchmark it on
  M-series before committing.
- Hashing is not the bottleneck at these speeds. A whole 4.6 GB `target/`
  hashes in about 2.4 s with single-thread SHA-256, and well under 1 s with
  multithreaded BLAKE3. Reading 400 to 2,000 source files cold from disk
  costs more. The win comes from *not reading* (section 1.4).

### 1.2 Which hash where

| Use | Requirement | Choice |
|---|---|---|
| CAS blob digest, action key, anything shared with another machine or user | collision resistance against adversarial inputs (cache poisoning) and stability forever | **BLAKE3-256**, optionally truncated to 128 bits for local-only indexes. Bazel ≥ 6.4 supports `--digest_function=blake3`, and BuildBuddy's cache/RBE accepts it ([BuildBuddy](https://www.buildbuddy.io/blog/how-bazel-7-0-makes-your-builds-faster)). Fall back to SHA-256 only for REAPI peers that lack BLAKE3. |
| Local "has this file changed" check, behind a stat cache | speed and stability across runs; no adversary | XXH3-128 or rapidhash. If the value ever becomes a key input, use BLAKE3 instead so there is one source of truth. |
| In-memory maps inside the daemon | latency on short keys | foldhash (hashbrown's default hasher), or rustc's FxHash. Keys are already digests, so an identity/truncation hasher on the first 8 bytes is the fastest option. |
| Chunk boundaries (CDC) | rolling hash | Gear hash (FastCDC) |

Rule: one cryptographic digest type in every persisted or shared structure.
Mixing digest types causes the keyspace confusion that REAPI handles with
`digest_function` fields.

### 1.3 How rustc and cargo hash today

- **rustc:** `StableHasher` wraps `SipHasher128` (a 128-bit copy of std's
  SipHash), with endianness and usize width normalized so results match
  across platforms. It is "very hot", and the buffer size was tuned against
  rustc-perf ([sip128.rs](https://doc.rust-lang.org/stable/nightly-rustc/src/rustc_data_structures/sip128.rs.html),
  [rustc-stable-hash](https://docs.rs/rustc-stable-hash),
  [PR #68914](https://github.com/rust-lang/rust/pull/68914) gave large
  incremental speedups just from inlining `short_write`). Query results get a
  `Fingerprint` (128-bit). The crate's SVH/crate hash summarizes the
  crate's metadata for downstream crates [memory].
  `-Zincremental-verify-ich` recomputes the hash of results loaded from the
  incremental cache and panics if a result marked green doesn't match its
  stored fingerprint ([verify_ich.rs](https://rust.googlesource.com/rust/+/refs/heads/main/compiler/rustc_middle/src/verify_ich.rs)).
  That is a ready-made correctness oracle for tinymold's cache-poisoning tests.
- **cargo:** Freshness has two parts.
  1. A *fingerprint* hash that covers rustc version, profile, features,
     target, dependency fingerprints, RUSTFLAGS, `[lints]`, and similar.
  2. *mtime* comparison of every source in the dep-info file against the
     dep-info file's own mtime, which is rewound to build start
     (`invoked.timestamp`). Registry and git deps skip mtime checks entirely.

  RUSTFLAGS is excluded from `-C metadata` (the symbol hash) because it often
  contains absolute paths, so changing RUSTFLAGS rebuilds without renaming
  files ([fingerprint docs](https://doc.rust-lang.org/nightly/nightly-rustc/cargo/compiler/fingerprint/index.html),
  [Metadata](https://doc.rust-lang.org/stable/nightly-rustc/cargo/compiler/struct.Metadata.html)).
  `# env-var:` lines in dep-info trigger rebuilds when an env var used by
  `env!` changes.
- **`-Zchecksum-freshness`** ([cargo#14136](https://github.com/rust-lang/cargo/issues/14136)):
  replaces mtimes with size and then a checksum that rustc emits via
  `-Zchecksum-hash-algorithm` (BLAKE3 available). Build-script inputs still
  use mtimes. This matters for tinymold. If rustc already records content
  hashes of every input in dep-info, the cache key can reuse them, and
  worktree checkouts (which reset mtimes) stop causing false rebuilds.

### 1.4 Avoiding hashing entirely

- **Stat cache:** Key a file by (dev, inode, size, mtime_ns, ctime_ns), map it
  to a digest, and only rehash when that tuple changes. Git's index does
  this, including "racy git" protection: if a file's mtime is not older than
  the index write time, the cached hash isn't trusted [memory]. Watchman and
  Buck2 keep the same table in a daemon. zccache keeps size, mtime and hash
  in a lock-free `DashMap`, so computing a key is a memory lookup
  ([zccache](https://libraries.io/cargo/zccache)).
- **Change journals instead of stat-walks:** FSEvents (macOS),
  inotify/fanotify (Linux), and Watchman's clock-based "since" queries give
  an O(changes) dirty set. Buck2 reads the Watchman merge-base at daemon
  start ([Buck2 options](https://buck2.build/docs/developers/options)). On
  every event-queue overflow, fall back to a full stat walk.
- **Filesystem- or VCS-provided hashes:** EdenFS serves SHA-1 or BLAKE3 for
  unmodified files from source-control metadata, so it never reads them.
  Watchman can return `content.sha1hex` [memory]. Git blob OIDs (SHA-1 or
  SHA-256 over `blob <len>\0` content) are available for free for clean
  tracked files via `git ls-files -s` plus `git status`. That makes a
  "VCS-clean ⇒ trust blob OID" fast path possible, at the cost of a second
  digest namespace that has to be mapped to BLAKE3 once per blob and then
  memoized. APFS and btrfs don't expose usable content hashes (btrfs has
  per-block crc32c/xxhash/sha256/blake2 checksums, not file digests)
  [memory].

### 1.5 Early cutoff and output hashing

Hash the *outputs*. If a rebuilt crate's `.rmeta` is byte-identical to the
previous one, downstream crates don't need to be re-checked. Their key
includes the dependency's rmeta *digest*, not its key (Bazel, Shake, Buck2
DICE "early cutoff",
[Modern DICE](https://buck2.build/docs/insights_and_knowledge/modern_dice/),
and Nix content-addressed derivations).

Cargo doesn't do this today. A dependency's fingerprint change propagates
by hash, so any edit to a crate rebuilds all its dependents, even a
comment-only edit in a private function (which doesn't change rmeta, given
deterministic output). This is the single most valuable use of hashing in
tinymold. It requires reproducible rmeta, which in turn requires stable
paths (`--remap-path-prefix`) and no embedded timestamps.

### 1.6 Content-defined chunking

FastCDC (Gear rolling hash, cut-point skipping, normalized chunking) is
3–12× faster than earlier CDC methods at a similar dedup ratio
([USENIX ATC'16](https://usenix.org/node/196197),
[TPDS 2020](https://ranger.uta.edu/~jiang/publication/Journals/2020/2020-IEEE-TPDS(Wen%20Xia).pdf)).
GB/s claims for Rust crates are targets, not measurements
([chunkrs](https://docs.rs/crate/chunkrs/latest)).

Applicability:

- A small edit to a 600 MB crate changes many codegen units, but most
  `.o` members and the rmeta tables are shifted copies. CDC with 16–64 KiB
  chunks should dedup successive versions of the same crate, and of test
  binaries that statically include the same dependencies. This is
  **unmeasured**. tinymold should measure it before building it.
- Use CDC for the cold/LAN tier and for transfers, not for the hot tier.
  rustc and the linker want whole contiguous files that can be mmap'd.

---

## 2. Persistent daemons and remote caches

| System | What stays resident | Lesson for tinymold |
|---|---|---|
| Bazel server + persistent workers | Action graph, file digests, warm JVM compilers. Java workers give 2–4× speedups ([Bazel docs](https://bazel.build/versions/6.0.0/remote/persistent), [2015 blog](https://blog.bazel.build/2015/12/10/java-workers.html)) | Warm *processes* pay off when startup or JIT dominates. rustc start is cheap, so the payoff has to come from warm *state* (section 3). |
| Gradle daemon + configuration cache | JVM, plus a serialized task graph that skips the configuration phase [memory] | Cache cargo's *unit graph* (`cargo build --unit-graph`) per (Cargo.lock, features, profile). Resolution and fingerprint walks cost real time on no-op builds. |
| Buck2 daemon (DICE) | Fine-grained incremental graph with early cutoff, Watchman-driven invalidation ([DICE slides](https://buck2.build/assets/Modern_DICE.pdf)) | The model to follow: dirty set from FS events, then recompute only along changed edges, with cutoff on equal values. |
| sccache server | Local server stays up 10 min idle. Per-crate cache keyed on args, sources and env | Doesn't cache `bin`, `dylib`, `cdylib` or `proc-macro` crates, or incremental builds. Requires `--emit` with `link` ([sccache Rust.md](https://github.com/mozilla/sccache/blob/main/docs/Rust.md)). Absolute paths in args and dep-info cause cross-directory misses. Mozilla disabled incremental with sccache (clean rebuild 3:50 → 2:05) ([gecko commit](https://source-hub.etos-pro.ru/mozilla/gecko-dev/commit/3e303cdeb93c9bc770cdca14197f9b97bbb28dfe)). |
| zccache | Unix-socket daemon, prost framing, `notify` watcher, metadata `DashMap`, BLAKE3, hardlink hits, auto `--remap-path-prefix` to git root for sibling worktrees | ~1 ms warm hit vs ~170 ms for sccache [vendor]. Records explicit miss reasons (`path_outside_root`, `content_hash_mismatch`). Closest existing design to tinymold's. |
| kache | `RUSTC_WRAPPER`, BLAKE3 keys over normalized invocation, SQLite index, blob store, reflink → hardlink → copy restore, optional daemon for S3, LRU `gc` | ≈88% cross-clone key stability on Firefox Rust [vendor]. Disables incremental. ([kache](https://github.com/Porges/kache)) |
| Zig `--listen` | The compiler itself is the server (stdin or TCP protocol). `-fincremental --watch` cut a real app's rebuild from ~16 s to ~139 ms ([ziggit](https://ziggit.dev/t/making-jetzig-project-compilation-over-10000-faster/7515), [commit](https://git.medv.io/zig/commit/986a30e373f6b2f0da2de64570013c83cacc17b6.html)) | Shows what a compile server with in-memory semantic state achieves. Not available for rustc (section 3). |
| rust-analyzer / salsa | Salsa query database with durability levels: library inputs are "high durability" and skipped in revalidation [memory] | Durability classes map directly to tinymold tiers: registry deps (immutable), workspace crates (volatile). |
| Turborepo / Nx daemons | File-hash cache and project graph kept in a daemon. Task outputs in a local and remote CAS [memory] | Same pattern at task level: hash inputs, replay outputs and logs. |
| ccache | `base_dir` rewrites absolute paths to relative ones. Supports secondary/remote storage (HTTP, Redis) [memory] | Path normalization and multiple tiers are standard practice. Worktrees that don't share a parent need prefix maps ([Doris guide](https://doris.apache.org/community/developer-guide/be-multi-directory/)). |
| Go build cache | Action ID = hash(command line, env, input contents, tool binary). Separate output ID = hash(output). `GOCACHEPROG` (Go 1.21+) delegates storage to an external program ([cache docs](https://pkg.go.dev/cmd/go/internal/cache@go1.19), [buildid.go](https://tip.golang.org/src/cmd/go/internal/work/buildid.go)) | The cleanest model: *action key → output digest → blob*. The two-level split gives early cutoff and dedup for free. |
| REAPI (Bazel remote, BuildBuddy, NativeLink, bazel-remote) | CAS (blobs by digest) plus ActionCache (Action digest → ActionResult). `FindMissingBlobs`, `BatchUpdateBlobs`, ByteStream, `compressed-blobs/{compressor}` ([proto](https://github.com/bazelbuild/remote-apis/blob/main/build/bazel/remote/execution/v2/remote_execution.proto)) | Use REAPI for the LAN/remote tier, not a custom protocol. Existing servers (bazel-remote is an LRU disk cache; NativeLink and BuildBuddy also do execution) then work out of the box. The Action digest covers command digest, input-root digest, platform, timeout and salt. |

### 2.1 Keeping artifacts hot

- **The page cache is the RAM tier.** A dedicated RAM CAS duplicates the
  kernel's cache. What helps instead is keeping the hot artifacts' *pages*
  resident and avoiding copies on materialization.
- **Materialization options and their sharing properties:**
  - **Hardlink:** same inode, so it shares disk blocks *and* page cache. Cost
    is O(1). Risk: an in-place writer corrupts the CAS. Make blobs read-only
    (0444), verify on restore by size and mtime, and only hardlink outputs
    that rustc and cargo replace via unlink+create or rename. Cargo already
    hardlinks `deps/` → `target/debug` uplifts.
  - **Reflink (`clonefile` on APFS, `FICLONE` on btrfs/XFS):** COW blocks,
    safe against in-place writes. It's a new inode, so page cache is *not*
    shared [memory], and macOS clonefile costs per-extent metadata.
  - **Copy:** what sccache does. It costs O(bytes) and doubles disk use.
  - **Recommendation:** hardlink for immutable inputs consumed by later
    rustc and linker runs (`.rlib`, `.rmeta` in `deps/`). Reflink for
    anything a tool might touch. Copy only across devices.
- **No tmpfs on macOS.** A RAM disk via `hdiutil` works but competes with
  the page cache. On Linux, tmpfs counts against RAM (see the user's
  CLAUDE.md note on quota exhaustion). Prefer disk CAS plus page cache plus
  `madvise(WILLNEED)` or `fcntl(F_RDADVISE)` prefetch of the next build's
  likely inputs.
- **Client/server transport:** Use a Unix socket with a small binary framing
  (as zccache does), carrying keys and paths only, never artifact bytes. The
  daemon materializes files directly into the requester's `target/`. Shared
  memory is unnecessary when payloads are paths and digests.

### 2.2 Correctness and poisoning

- Keys must cover every input. Hidden inputs include: `env!` vars (listed in
  dep-info), files read by proc macros or `include_bytes!`
  (`-Zbinary-dep-depinfo` helps), build-script outputs, the linker and
  sysroot for bin crates, and rustc's own binary hash.
- **Verification modes:**
  1. Random-sample rebuild-and-compare (e.g. 1% of hits, re-executed in the
     background, alert on any mismatch). Bazel, Buck and Nix call this
     "--check" or "diff-ing".
  2. `-Zincremental-verify-ich` in CI for the incremental path.
  3. Never trust a remote tier for writes from untrusted clients. CI writes,
     developers read-only, or per-user namespaces via a REAPI `salt` or
     instance name.
- **Atomic publish:** write blob to temp → fsync → rename into CAS → write
  the action entry last. Readers validate the blob digest on first use per
  daemon session, then trust it.

---

## 3. Can a daemon keep rustc's query state warm?

What blocks it:

- rustc is one-shot by design. `rustc_interface::run_compiler` builds a
  `Session`, global arenas and an interner, and `TyCtxt<'tcx>`, and drops
  them all on exit. The `'tcx` lifetime and arena interning tie every query
  result to that one session. There's no API to apply source edits to a live
  `TyCtxt` [memory]. Global state lives in thread-locals and session globals
  (`rustc_span::SESSION_GLOBALS`, the symbol interner), and the query caches
  assume the inputs are frozen.
- `rustc_driver` and `rustc_interface` are unstable (nightly
  `rustc_private`). A long-lived embedder (Miri, Clippy, Kani) still runs
  one compilation per invocation.
- rust-analyzer gets warm incremental state by **reimplementing** the
  frontend on salsa (its own name resolution, type inference via
  chalk / the next-gen trait solver). It has no MIR borrowck or codegen, so
  it can't produce artifacts.
- What exists today is on-disk incremental. `-C incremental` persists the
  dep graph and query results (red/green marking). The cost on a warm build
  is loading and decoding the dep graph and query cache, and re-hashing
  inputs.

What's realistic (rising difficulty):

1. **Keep incremental directories hot, not processes.** Pin
   `target/*/incremental/<crate>` session dirs in page cache, and give each
   (crate, key-without-sources) its own incremental dir in the daemon's
   store. Then switching worktrees or branches doesn't discard state. This
   is cheap and works with stable rustc.
2. **Prespawned rustc.** Start a rustc process and stall it until a job
   arrives (fork-server style). This saves exec, dyld/ld.so and LLVM init,
   which is tens of ms per invocation [memory]. That matters on 400-crate
   graphs and on `check` builds. It needs a tiny `rustc_driver` shim and is
   nightly-only.
3. **Persistent-worker rustc via `rustc_driver`:** one process runs many
   `run_compiler` calls in sequence, reusing *process-level* warm state
   (allocator, loaded LLVM, file cache, mmap'd dependency rmeta). It can't
   reuse `TyCtxt`. This is roughly Bazel's persistent worker for rustc
   (rules_rust has experimented with this [memory]). It's moderate work but
   tracks nightly churn.
4. **True in-memory incremental (Zig-style):** requires upstream rustc work
   on a long-lived session with input mutation. Not realistic for tinymold
   alone. Watch the rustc parallel-frontend and incremental project goals
   instead.

---

## 4. Storage footprint

### 4.1 Where the bytes go [measured, this machine]

The target dirs were 4.6 GB (openhuman), 2.4 GB (chottu-core) and 1.5 GB
(openscion; it has extra `llvm-cov-target` and bench profiles). Breakdown of
openhuman `target/debug/deps`, 3.3 GB of the 4.6 GB total:

| Kind | Files | Size | Note |
|---|---|---|---|
| `.rlib` | 457 | 1,596 MB | one crate alone is 634 MB |
| `.rmeta` | 457 | 633 MB | **duplicated inside each rlib** (embed-metadata) |
| test/bin executables | 3 | 666 MB | debuginfo-heavy, re-linked per edit |
| `.o` | 85 | 545 MB | leftover codegen units |
| `.dylib` (proc-macros) | 24 | 71 MB | |
| `build/`, `.fingerprint/` | — | 66 MB, 9 MB | |
| `incremental/` | — | 0 | incremental off in this setup; when on, upstream data puts it at ~1.5–2× the no-incremental size (below) |

Upstream numbers. Measuring cargo's own target dir, the
[Inside Rust post (Aug 2026)](https://blog.rust-lang.org/inside-rust/2026/08/18/reducing-target-dir-size-on-nightly/)
gave these dev-profile sizes:

| Incremental | Debuginfo | Size |
|---|---|---|
| on | on | 3,151 MiB |
| on | off | 1,476 MiB |
| off | on | 2,066 MiB |
| off | off | 1,000 MiB |

`-Zembed-metadata=no` saves a constant ~240 MiB, which is 7.6% to 33% of
the total. So for a typical dev build, debuginfo is ~50% of the bytes and
incremental ~35%. Disabling debuginfo in nats.rs: 543 → 424 MB
([PR](https://github.com/nats-io/nats.rs/pull/1122)). On macOS, packed
debuginfo duplicates into `.dSYM`. `split-debuginfo=unpacked` avoids the
copy ([internals](https://internals.rust-lang.org/t/help-test-faster-incremental-debug-macos-builds-on-nightly/14016)).

### 4.2 Dedup potential across projects and worktrees [measured]

- **Two unrelated projects** (openhuman vs chottu-core): 144 rlibs share the
  same `name-<metadata hash>` filename, and **142 of those (83 MB) are
  byte-identical**. 323 crate *names* overlap, but only 144 hashes match:
  feature unification and version skew split the rest. A shared CAS
  therefore dedups registry deps across projects, and normalizing feature
  sets (section 5, idea 12) would increase that share.
- **Two worktrees of one project** (openscion main vs a worktree): of 209
  same-name `.rlib`/`.rmeta` files, 168 (122 MB) are identical and **41
  (65 MB) differ despite identical filenames**. Same filename does not imply
  same bytes, plausibly because RUSTFLAGS (excluded from `-C metadata`) or a
  toolchain/build-script difference wasn't captured. Cause not determined.
  This is why tinymold must key on full inputs and verify output digests
  rather than trust cargo's filename hash.
- **Worktree target dirs** were 353 MB and 568 MB, alongside 1.5 GB for
  main. With N agents × M worktrees, registry-dep artifacts are the bulk and
  almost entirely shareable.

### 4.3 Compression [measured]

On the 665 MB `libopenhuman_core` rlib:

| Setting | Output | Ratio | Wall time (multi-threaded) |
|---|---|---|---|
| `zstd -1 -T0` | 138 MB | 4.8× | 0.165 s |
| `zstd -3 -T0` | 126 MB | 5.3× | 0.14 s, ≈4.7 GB/s |

Rust artifacts compress very well (debuginfo, symbol strings, metadata).
Compress the cold and LAN tiers and network transfers (REAPI
`compressed-blobs/zstd`). Keep the hot tier uncompressed, because rustc and
the linker mmap rlibs and rmeta.

`-Zdebuginfo-compression=zstd` / `-C link-arg=-Wl,--compress-debug-sections=zstd`
shrink ELF debuginfo in place on Linux [memory]. Mach-O has no equivalent;
there, use `split-debuginfo=unpacked` and skip `dsymutil` for dev builds.

### 4.4 Garbage

- Incremental session dirs pile up per crate hash. cargo-sweep
  (`--time`, `--installed` toolchain filter, `-r`) cleans them, as does
  `cargo clean gc`/`-Zgc`, which covers only `CARGO_HOME` today. Its SQLite
  last-use tracker is the model to copy
  ([cargo cache cleaning](https://blog.rust-lang.org/2023/12/11/cargo-cache-cleaning/),
  [cargo#12633](https://github.com/rust-lang/cargo/issues/12633)).
  Target-dir GC is still being designed: ~35 ms tracking overhead on HDD
  for 200 deps ([design notes](https://hackmd.io/@rust-cargo-team/SJT-p_rL2)).
- Old toolchain and profile artifacts are never reclaimed by cargo.
  Anecdotally, 773 GiB was freed across 127 target dirs
  ([cargo-clean-all](https://github.com/dnlmlr/cargo-clean-all)).
- `-Zbuild-dir-new-layout` ([cargo#15010](https://github.com/rust-lang/cargo/issues/15010))
  regroups the build dir per unit, which makes per-unit eviction and locking
  feasible. tinymold should target that layout.

### 4.5 Size budget proposal

| Tier | Contents | Budget (default) | Eviction |
|---|---|---|---|
| Hot (page cache, pinned hints) | rmeta and rlib of the current graph of each *active* worktree, plus the last test binary | ≤ 25% of RAM, e.g. 8 GB on 32 GB. Openhuman's hot set ≈ 2.2 GB (rlib + rmeta) | Advisory only (`F_RDADVISE`/`WILLNEED`), driven by active-workspace heartbeats |
| Warm (local CAS, uncompressed, hardlinked into targets) | All artifacts of the last K builds per workspace and toolchain | 30 GB, or 10% of free disk | LRU by *last materialization*, with per-project quota and a floor so each active workspace keeps its latest complete graph |
| Cold (local zstd-compressed, optional CDC) | Evicted warm blobs, old toolchains | 20 GB compressed (≈100 GB logical at 5×) | LRU and age (14 days) |
| LAN/remote (REAPI) | CI-produced artifacts | Server-side | Server LRU (bazel-remote) |

Target outcome: N worktrees cost roughly 1× registry deps plus N× workspace
crates, instead of N× everything. On this machine that is ~2 GB + 0.3 GB × N
rather than ~2.5 GB × N.

---

## 5. Fifteen design decisions for tinymold

| # | Decision | Expected impact | Difficulty |
|---|---|---|---|
| 1 | **Two-level cache (Go/REAPI model):** action key → output manifest (digests) → CAS blobs | Dedup, early cutoff, remote compatibility. This is the foundation. | Medium |
| 2 | **BLAKE3-256 for every persisted or shared digest**, XXH3 only behind the stat cache, foldhash or identity for in-RAM maps | Safe sharing. Hashing never shows up on profiles. | Low |
| 3 | **Daemon with a stat cache and FS-event dirty set** (FSEvents/inotify, Watchman optional). Unix-socket protocol carrying paths and digests only | Key computation drops from tens–hundreds of ms to ~1 ms (zccache-like). No-op builds become near-instant. | Medium |
| 4 | **Early cutoff on rmeta digest:** downstream keys use the dependency's *rmeta output digest*, not its key | Edits that don't change the public interface skip all dependents' rebuilds. Largest win on deep graphs. | Medium–High: needs reproducible rmeta and cargo integration (wrapper or `-Zbuild-plan`-style driver) |
| 5 | **Path normalization:** inject `--remap-path-prefix` for workspace root, `CARGO_HOME` and sysroot. Key on relative paths. Verify that outputs contain no residual absolute paths. | Cross-worktree, cross-agent and cross-machine hits (kache reaches 88% key stability on Firefox) | Medium (debuginfo and `file!()` subtleties) |
| 6 | **Hardlink materialization from a read-only CAS**, reflink fallback, copy only across filesystems | O(1) restores. Shared disk and page cache across worktrees. | Low–Medium (guard against in-place writers) |
| 7 | **Cache bin, test, cdylib and proc-macro crates too**, which sccache can't. Include linker identity and args, and the digests of every linked rlib, in the key. | Cached test binaries remove the ~0.7 GB re-link per idle switch | Medium |
| 8 | **Per-key incremental-dir store:** keep `incremental/` dirs keyed by (crate, non-source key) in the daemon, swapped in on branch or worktree switch | Turns incremental cold starts after a switch into warm ones. Fixes the sccache-vs-incremental conflict. | Medium |
| 9 | **Reuse rustc's own checksums** (`-Zchecksum-hash-algorithm=blake3` dep-info) where nightly is available, own hashing on stable | Single source of truth for input digests, and fewer reads | Low |
| 10 | **Output verification sampling** (rebuild 1% of hits in the background and compare digests), plus a `--verify` CI mode with `-Zincremental-verify-ich` | Detects nondeterminism and poisoning early, as the openscion same-name/different-bytes case shows is needed | Low–Medium |
| 11 | **REAPI client for the LAN/remote tier** with `compressed-blobs/zstd` and `FindMissingBlobs` batching. CI writes, developers read. | Reuses bazel-remote, NativeLink and BuildBuddy. Fresh clones hit within minutes. | Medium |
| 12 | **Feature-set canonicalization report:** detect the same crate and version built under divergent feature sets across workspaces, and suggest unifying them | Raises cross-project hit rate (only 144 of 323 shared crates matched) | Low (reporting) / High (automatic) |
| 13 | **Tiered storage with quotas:** hot (page-cache hints), warm (uncompressed CAS, LRU by last materialization, per-project floor), cold (zstd, ~5×). Global budget in config. | Bounded disk. Up to ~4–5× fewer bytes for N worktrees. | Medium |
| 14 | **Storage hygiene defaults:** `-Zembed-metadata=no` (−240 MiB on cargo), `split-debuginfo=unpacked` on macOS, `debug="line-tables-only"` for deps, and debuginfo zstd on Linux | 30–50% smaller artifacts. Faster links. | Low (profile config) |
| 15 | **Prespawned / persistent `rustc_driver` worker** (nightly opt-in), reusing process-level warm state (LLVM init, mmap'd dependency rmeta) | Tens of ms per invocation × hundreds of crates for check builds. True `TyCtxt` reuse is out of reach. | High (tracks nightly API churn) |

---

## 6. Proposed cache-key schema for one rustc invocation

```text
ActionKey = BLAKE3-256( canonical_cbor({
  schema:        "tinymold/rustc/v1",
  tool: {
    rustc_digest:     BLAKE3(rustc binary) + `rustc -vV` (commit-hash, host, LLVM),
    sysroot_digest:   Merkle(BLAKE3 of target's std rlibs/rmeta),
    linker:           { path_rel, digest, version }   # bin/test/cdylib/dylib only
  },
  target:        target triple | BLAKE3(target-spec JSON),
  crate: {
    name, crate_types[], edition, emit[],             # emit includes metadata/link/dep-info
    root_src_rel:     path relative to workspace root
  },
  args_normalized: [ every rustc arg, in order, with:
      - absolute paths → "${WS}/..", "${CARGO_HOME}/..", "${SYSROOT}/..", "${OUT}/.."
      - --out-dir, -C incremental, --error-format/--json/--color dropped
        (outputs are keyed separately)
      - -C metadata / -C extra-filename kept (they affect symbols)
      - --remap-path-prefix injected by tinymold, excluded from key ],
  cfg_and_features: sorted --cfg and --check-cfg values,
  rustflags_source: sorted list (RUSTFLAGS / config / [lints]) normalized as above,
  env: sorted { k: v } for
      - every `# env-var:` in dep-info of the previous build of this action
        (two-phase: lookup by a "pre-key", then confirm with recorded env set)
      - CARGO_PKG_*, CARGO_MANIFEST_DIR (normalized), CARGO_CRATE_NAME,
        OUT_DIR (normalized), RUSTC_BOOTSTRAP, SOURCE_DATE_EPOCH, CARGO_CFG_*,
  inputs: sorted [ (path_rel, BLAKE3(content)) ] for every file in dep-info
      (incl. include_str!/include_bytes!, -Zbinary-dep-depinfo extras),
  externs: sorted [ (crate_name, BLAKE3(rmeta output) ) ]       # early cutoff:
      # rmeta digest for check/metadata-pipelined deps; rlib digest only when
      # this action links (bin/test/cdylib)
  proc_macros: sorted [ (name, BLAKE3(dylib)) ],
  build_script_out: Merkle(OUT_DIR contents) + rerun-if-* declared inputs,
  profile_bits: { opt-level, debuginfo, lto, codegen-units, panic, debug-assertions,
                  overflow-checks, split-debuginfo, strip }       # usually in args already
}))

ActionResult (ActionCache entry) = {
  outputs: [ (kind: rlib|rmeta|bin|dylib|d|dwo|dSYM-dir, path_rel, BLAKE3, size, exec_bit) ],
  stdout/stderr digests (diagnostics replay, with ${WS} re-expanded),
  recorded_env_names: [..],          # validates the two-phase env lookup
  dep_info_inputs: [..],             # for next lookup's prekey
  producer: { host, tinymold_version, time }, verified: bool
}
```

Key rules:

- **Two-phase lookup:** the pre-key excludes `inputs` and `env`. It maps to
  the recorded input-path and env-name set, and the final key is computed
  from those. This is how ccache's "direct mode" and sccache handle
  dependencies discovered only after compilation.
- **Not in the key:** mtimes, absolute workspace path, the incremental
  dir, jobserver, `CARGO_MAKEFLAGS`, and terminal or color flags.
- **Incremental builds:** never store artifacts produced from a reused
  incremental session into the shared tier unless output verification
  (decision 10) has confirmed equivalence. Store them in the local tier
  marked `verified: false`.
