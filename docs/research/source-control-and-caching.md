# Source control, VFS, and content-addressed caching: research for tinymold

Date: 2026-10-14. Scope: what server-centric source control (Anthropic's Scion,
its open re-implementation OpenScion) and the big-company prior art (Google,
Meta, Microsoft, REAPI, Nix) teach about cutting build time, and what tinymold
can take from it for Rust/cargo with many parallel worktrees and AI agents.

Legend for confidence: **[src]** = read in the primary source during this
research; **[known]** = well-established public knowledge, not re-verified
today; **[unverified]** = plausible but not confirmed.

---

## 1. OpenScion (github.com/tinyhumansai/openscion)

Source: https://github.com/tinyhumansai/openscion (README, `docs/specs/*`,
`docs/research/scion.md`, `benchmarks/README.md`, crate sources; shallow clone,
not built).

### What it is

An open-source, server-centric source control system for very large monorepos
whose clients are mostly AI agents and CI jobs. It is an explicit
re-implementation of the design Anthropic described for Scion. Rust 2024
workspace, same template conventions as tinymold (proto crate + core crate +
server + CLI `opnscn`, plus a Next.js web GUI and a benchmark crate). **[src]**

Status (ROADMAP): only phase 1 is done: in-memory core data model, object /
changeset / ref routes over HTTP JSON, `opnscn import | commit | clone`
(including single-directory clone), and a git-vs-OpenScion benchmark. Derived
data, server-side rebase/land, persistent stores, the daemon, the regional
cache, git ingestion, and FUSE checkouts are all future phases. **[src]**

### Architecture

```text
opnscn (client) -> openscion-daemon -> openscion-cache -> openscion-server
working copy      one per node        regional,           log, refs,
fs watcher        node-wide objects   immutable objects   derived data
                                                    metadata store + object store
```

- **Uniform RPCs at every tier.** Daemon, cache, and server answer the same
  object RPCs, so a client talks to the nearest one. Tiering is strict: each
  tier calls only the one above, bounds upstream concurrency, and sheds load
  with `Unavailable` instead of skipping a tier. Mutable queries (refs, log,
  derived data) always go to the server and are never cached. **[src]**
- **Two storage ports.** `ObjectStore` (put-if-absent, get-by-id) and
  `MetadataStore` (append-only log with monotonic LSNs, point lookups, CAS on a
  single ref record). No cross-record transactions, so metadata can shard.
  Reference impls: in-memory, filesystem, SQLite. **[src]**
- **Daemon snapshot boot.** The per-node daemon boots from a periodically
  refreshed snapshot of recently used objects so a fresh node is not cold. **[src]**

### Data model (byte-exact, durable)

- `ObjectId = BLAKE3(kind ‖ ' ' ‖ decimal(len) ‖ 0x00 ‖ payload)`, kinds
  `blob | tree | changeset`. Hash over the *uncompressed* payload so storage
  compression can change without changing ids. BLAKE3 chosen for speed and for
  verified streaming of large blobs later. **[src]**
- Trees: sorted entries `kind-byte ‖ name ‖ 0x00 ‖ 32-byte id`, kinds
  file / executable / symlink / tree. Trees are **derived by the server**,
  never written by clients. **[src]**
- Changesets: parents + file-level `Upsert{path,kind,blob}` / `Delete{path}`
  relative to the **first parent only**, plus author, message, server-assigned
  `created_at`, and an open metadata map. Cost scales with the change, not the
  repo. Root trees are derived data computed by path-copying from the parent's
  tree. **[src]**
- Refs: few (`main`, `release/*`), moved only by compare-and-swap; creating a
  changeset never touches a ref. **[src]**

### Working copy (the part most relevant to builds)

`crates/openscion/src/workcopy/mod.rs` keeps `.opnscn/state.json` with each
file's blob id, size, and mtime. A scan walks the directory; a file whose stat
data matches and whose mtime is older than the state's `written_at` is not
re-hashed (git's racily-clean rule). Today this is still a **stat-walk**; the
spec promises a filesystem watcher in phase 5 and FUSE checkouts in phase 8.
**[src]**

### Benchmarks (in-memory, early; authors flag heavy caveats)

| Scenario | git 1k | OpenScion 1k | git 10k | OpenScion 10k |
| --- | ---: | ---: | ---: | ---: |
| import | 425 ms | 21.9 ms | 3,966 ms | 245 ms |
| commit-one-file | 66.9 ms | 2.5 ms | 21.8 ms | 22.3 ms |
| full-clone | 241 ms | 50.7 ms | 2,343 ms | 544 ms |
| sparse-clone | 84.2 ms | 6.1 ms | 158 ms | 25.3 ms |

Caveats (README): no disk, fsync, compression, process spawn, or packing on
the OpenScion side; at 10k files its JSON state file is slower than git's
binary index. The durable lesson is the *shape*: sparse-clone cost tracks the
slice requested, not repo size. **[src]**

### Ideas from OpenScion relevant to tinymold

- **BLAKE3 Merkle tree ids** give a crate's source directory a verifiable,
  location-free content address: an ideal cache-key component.
- **Tiered, coordination-free caches with identical RPCs** (process -> node
  daemon -> regional -> origin) is exactly the shape of a build artifact cache;
  immutable entries need no invalidation. **Snapshot boot** pre-warms new boxes.
- **Stat-cache with racily-clean rule** is the no-watcher fallback; use a
  binary index (the JSON state lost to git at 10k files).
- **Derived data defined recursively from parents** is the incremental-build
  pattern: value(node) = f(own inputs, parents' values).

---

## 2. Anthropic, "Redesigning source control" (Scion)

Source: https://www.anthropic.com/engineering/redesigning-source-control **[src]**

### Problem

Merged changes grew from ~2,500/day (May 2026) to >30,000/day (Sept 2026).
"The bottleneck, now, is all of the developer infrastructure built for human
scale." Main history ~100 GB; compressed working copy >25 GB; ~25 M CI
checkouts/day (excluding dev envs, agent sandboxes, research); naive fetch
>600 PB/day; several hundred git mirrors at peak. Shallow/sparse/promisor/
mirror/seed workarounds strained (seeds drifted from main; diff/blame on
partial clones broke). Root cause: git's offline full-copy model used as a
centralized system by always-online clients.

### Design (six principles)

1. **Always-online clients** fetch on demand, no full copies.
2. **Client/server asymmetry**: server does history, blame, commits, merges,
   rebases; client keeps a bounded cache, lays out files, **watches the
   filesystem**, provides ergonomics.
3. **Source control as a rich API**: many jobs need no local repo; automated
   bisect/revert pipelines depend on it.
4. **Horizontal scalability**: no full materialized copy anywhere, not even on
   the server; no packfiles, therefore no repo maintenance.
5. **Cheap, incremental, lock-free commits**: server-side, cost proportional to
   change size; refs discouraged beyond main/release (global state is a
   scaling problem).
6. **Optional, sparse working copy**: many CI jobs skip checkout (ask for a
   diff vs merge base); checkouts are sparse or **virtualized, materializing
   files on demand**.

### Data model and storage

Changesets live in a write-ahead log, created server-side and visible everywhere
at once, listing files changed vs the first parent with content-addressed
pointers to compressed blobs. Refs are a CAS on one Spanner row, decoupled from
changeset creation. **Derived data** (blame, path history, commit-graph skip
lists, copy detection, path search) uses one generic mechanism, each type
defined recursively from parents' values, computed eagerly or lazily, invisible
to clients. Spanner holds metadata/refs, blob storage holds contents; because
everything is content-addressed and immutable, "any server can cache it without
coordination".

### Monorepo scale: tiers and fetch

- **scion-daemon** per node (socket; shares objects among all jobs on the
  node; boots from a refreshed snapshot); **scion-cache** regional (streams
  from memory in the daemon's disk format, no computation, no delta
  compression); **scion-server** authoritative. Identical RPCs at every tier;
  strict tiering isolates failure domains.
- Fetch: the client does not tell the server what it has. It names the trees
  and blobs it wants, starting from a root tree, recursing on missing trees,
  then blobs. Every request is for an immutable object, so every cache can
  answer it and transfers are resumable. Sparse and virtual checkouts "need no
  special support".

### Merge queue, review, security

No global merge-queue lock (post-merge and pre-deploy tests catch breakage;
Claude fixes conflicts fast). Antique review tool expresses semantic approval
rules (comment-only vs sensitive logic). Per-directory permissions.

### Results

~200x smaller CPU/memory than the git mirror fleet; CI and research checkouts
~100x faster (minutes -> seconds); ~25% CI CPU capacity regained; >99% cache
hit rate at every tier. Seven people, five months; a model rewrote the git
ingestion pipeline with 20x less memory and 4x speed.

### Gaps and caveats

No mention of FUSE by name, Bazel, build caching, or test caching; CI appears
only as a consumer of checkouts and diffs. The build lessons are indirect: the
25% CI CPU win came from not spending CPU on checkouts, and the architecture
(content addressing, node daemon, tiered caches, diff-without-checkout)
transfers directly to build artifacts. Authors: "Scion is by no means a
suitable replacement for git for general use"; they ask forges for server-side
RPCs, a new negotiation protocol, and sparse/shallow features that compose.

---

## 3. Prior art

### 3.1 Google: Piper + CitC + Blaze/Bazel

Sources: Potvin & Levenberg, "Why Google Stores Billions of Lines of Code in a
Single Repository", CACM 2016 (https://cacm.acm.org/research/why-google-stores-billions-of-lines-of-code-in-a-single-repository/);
https://en.wikipedia.org/wiki/Piper_(source_control_system); Bazel blog posts below.

- **Piper**: centralized VCS on Spanner/Bigtable; 2015 snapshot ~2 B lines,
  ~9 M files, ~35 M commits, ~86 TB. **[known]**
- **CitC** (Clients in the Cloud): a cloud backend plus a Linux FUSE
  filesystem showing the whole repo at a revision with the user's edits
  overlaid; a workspace stores only modified files (typically <10 files).
  Workspaces are cheap to create, so many per user is normal. **[known]**
- **Blaze/Bazel integration**: the build system reads file metadata and
  digests from the VFS instead of reading files; builds run on remote
  execution (Forge) with a global content-addressed action cache; outputs stay
  remote ("build without the bytes"). In open-source Bazel: **[known/src]**
  - Action cache key = digest of (command line, env, input Merkle tree,
    platform). Outputs in a CAS. This is the REAPI model (3.4).
  - `--unix_digest_hash_attribute_name`: Bazel can read a file's digest from an
    **extended attribute** instead of hashing the file; added for read-only
    corpora and usable by FUSE filesystems that already know the digest.
    https://bazel.googlesource.com/bazel/+/master/src/test/shell/bazel/unix_digest_hash_attribute_name_test.sh **[src]**
  - `--watchfs`: use OS file notifications instead of stat-walking. **[known]**
  - **Builds without the Bytes** (default since Bazel 7): intermediate outputs
    stay remote, only top-level outputs downloaded.
    https://blog.bazel.build/2023/10/06/bwob-in-bazel-7.html **[src]**
  - **Remote Output Service** (Bazel 7.2+): `bazel-out/` is a FUSE (Linux) or
    NFS (macOS) virtual filesystem; outputs look local and are fetched only when
    read. `bb_clientd` implements it and proxies REv2.
    https://blog.bazel.build/2024/07/23/remote-output-service.html **[src]**

### 3.2 Meta: Sapling + EdenFS + Watchman + Buck2

Sources: https://sapling-scm.com ; EdenFS overview (mirror)
https://github.com/scramsby/sapling/blob/main/eden/fs/docs/Overview.md ;
https://engineering.fb.com/2023/04/06/open-source/buck2-open-source-large-scale-build-system/ ;
https://buck2.build/assets/Modern_DICE.pdf

- **Sapling** client + **Mononoke** server; segmented changelog lets clients
  hold only a sparse view of the commit graph. **[known]**
- **EdenFS**: virtual filesystem (FUSE on Linux, NFS/FSKit on macOS, ProjFS on
  Windows) that lazily materializes files from source control; unmodified
  files are served from the object store. It (a) delivers change
  notifications through **Watchman** "to allow downstream tools like build
  tools and IDEs" to react, and (b) **returns file content hashes without
  reading the file**, because it already knows the blob hash for every
  unmodified file. **[src]**
- **Watchman**: daemon that answers "what changed since clock X?", turning an
  O(repo) stat walk into an O(changes) query; on EdenFS it uses Eden's journal
  rather than kernel notifications. **[known]**
- **Buck2**: "written to integrate with virtual file systems"; "we watch for
  file notifications (with Watchman) and request both files and file-digests
  without direct file operations", and can "efficiently compute recursive
  digests" (directory digests from Eden's trees). Single incremental
  dependency graph, no phases; builds ~2x faster than Buck1; speaks the same
  REAPI as Bazel. **[src]**
- **DICE**: Buck2's dynamic incremental computation engine. Parallel,
  dependency-tracked computations; some leaves (file reads) are invalidated
  externally by the watcher; recomputation stops early when a node's value is
  unchanged (early cutoff). Graph state persists across commands in the buck2
  daemon. **[src: Modern DICE slides; early cutoff known]**

### 3.3 Microsoft: VFS for Git -> Scalar

Sources: https://github.blog/open-source/git/the-story-of-scalar/ ;
https://github.com/microsoft/VFSForGit (FAQ).

- **VFS for Git (GVFS)**: ProjFS virtual working tree for the Windows repo;
  hydrates files on first open; its own virtualization-aware watcher so git
  skips unhydrated files. Still supported for the Windows OS repo. **[src]**
- macOS kext deprecation pushed Office to **sparse-checkout**. **Scalar** (core
  git since 2.38) bundles partial clone, cone-mode sparse-checkout + sparse
  index, built-in **fsmonitor**, commit-graph, multi-pack-index, background
  maintenance. Lesson: virtualization is platform-fragile; the portable 80% is
  *sparse + partial clone + fsmonitor*. **[src/known]**

### 3.4 Remote Execution API (REv2), BuildBuddy, NativeLink, others

Sources: https://github.com/bazelbuild/remote-apis ;
https://buck2.build/docs/users/remote_execution ;
https://github.com/TraceMachina/nativelink ; https://www.buildbuddy.io

- **CAS** stores blobs by digest (SHA-256 default; BLAKE3 allowed in newer
  REv2); directories are Merkle trees. **Action** = digest(Command, input-root
  digest, platform); **ActionCache** maps it to an ActionResult (output digests,
  exit code, stdio). Canonicalization makes equal actions hash equally;
  **FindMissingBlobs** uploads only what the server lacks. **[src]**
- Servers: BuildBuddy, NativeLink (Rust; CAS + AC + scheduler + workers),
  EngFlow, Buildbarn, BuildGrid, bazel-remote. Clients: Bazel, Buck2, Pants,
  **recc** (wraps one compiler invocation, ccache-style), Reclient/Siso. **[src/known]**

### 3.5 Nix content-addressed derivations

Sources: https://www.tweag.io/blog/2021-12-02-nix-cas-4/ ; https://wiki.nixos.org/wiki/Ca-derivations

- Input-addressed Nix keys outputs by the recipe hash, so an upstream rebuild
  with identical bytes still rebuilds everything downstream. CA derivations
  (RFC 62, experimental since Nix 2.4) "resolve" a derivation by substituting
  inputs' content addresses, so identical outputs give dependents a cache hit
  (**early cutoff**) and untrusting users can share a store. **[src]**
- Rust mapping: `.rmeta` is a crate's interface. Keying dependents by the
  *content* of upstream `.rmeta` means a private body edit stops at one crate.

### 3.6 Cargo/rustc today (where the gaps are)

Sources: https://doc.rust-lang.org/cargo/reference/unstable.html **[src]**;
sccache docs https://github.com/mozilla/sccache **[known]**

- Freshness uses **mtimes**; `-Z checksum-freshness` (cargo#14136) uses file
  checksums instead (build-script inputs still use mtimes). Survives worktree
  copies that bump mtimes, but cargo still reads and hashes every file.
- `-Z build-dir-new-layout` (#15010) and `-Z fine-grain-locking` (#4282)
  replace the whole-target-dir lock and "unblock work towards caching"; today a
  shared target dir across worktrees serializes on one lock.
- Workspace members are path deps: unit metadata hashes and embedded paths
  depend on the **absolute path**, so the same crate in two worktrees is two
  units. Registry deps share; workspace crates do not. **[known]**
- **sccache** keys per rustc invocation on args + input hashes + env; it
  cannot cache incremental builds or linker-invoking crates (bin, dylib,
  cdylib, proc-macro); absolute paths hurt cross-checkout hits. No cargo
  action cache with early cutoff exists; rustc incremental state is local and
  path-sensitive. **[known]**

---

## 4. How source control + VFS integration speeds builds (synthesis)

| Mechanism | Who | Build-time win |
| --- | --- | --- |
| Change journal instead of stat walk | Watchman/EdenFS, git fsmonitor, Bazel `--watchfs`, Scion client | Null/no-op build cost O(changed files), not O(repo). Matters at 10^5+ files and for agents that rebuild constantly. |
| Hashes for free from the VFS | EdenFS -> Buck2; Bazel xattr digests; CitC -> Blaze | No reading/hashing of unchanged sources; directory digests are just tree ids. |
| Merkle trees = cache keys | REAPI input roots; Scion/OpenScion trees | A crate's source tree id plus dep interface ids is a stable, location-free action key. |
| Lazy materialization | CitC, EdenFS, VFS for Git, Bazel Remote Output Service | New worktree/agent sandbox in seconds; outputs pulled only when read. |
| Shared node daemon + tiered CAS | scion-daemon/cache, bb_clientd, BuildBuddy/NativeLink | One copy of objects/artifacts per box; >99% hit rates; no invalidation since content is immutable. |
| Server-side diff vs merge base | Scion API, Piper | CI/agents select affected targets without a checkout. |
| Early cutoff on content | Nix CA, DICE, Bazel (output digests) | Edits that don't change an interface stop propagating. |
| Persistent daemon graph | Buck2 daemon, Bazel server | No re-parsing/re-planning per command. |

The common thread: **once every file and directory has a content address that
source control already computed, the build system never needs to read the
disk to decide what is stale, and every output can be keyed by content so any
worktree, agent, or machine can reuse it.**

---

## 5. Ideas for tinymold (Rust/cargo, many worktrees, many agents)

Impact = expected wall-clock reduction for the target workflow (N agents, M
worktrees of one repo, frequent small edits). Difficulty: S (days), M (weeks),
L (months), XL (quarter+ / upstream rustc/cargo work).

1. **Location-independent unit keys for workspace crates.** Key every rustc
   invocation by (rustc version, target, profile/flags with paths remapped,
   features, source-tree BLAKE3 Merkle id, dependency `.rmeta` digests, env
   allowlist). Use `--remap-path-prefix` / `-Z remap-path-scope` so outputs
   are byte-identical across worktree paths. Makes worktree #2..#N of the same
   commit near-free. *Impact: very high* (today workspace crates never share
   across worktrees). *Difficulty: M.*

2. **Node-local artifact daemon (the scion-daemon of builds).** One process per
   machine owning a CAS + action cache on local disk, reached over a Unix
   socket by a `RUSTC_WRAPPER` shim from every worktree and agent; hardlink or
   reflink (APFS `clonefile`, Linux `FICLONE`) outputs into each `target/`
   instead of copying. Replaces "one shared target dir with one lock".
   *Impact: high. Difficulty: M.*

3. **Tiered, REAPI-compatible remote cache.** Make the daemon a read-through
   client of an REv2 CAS/AC (NativeLink, BuildBuddy, bazel-remote) so teams and
   CI share hits; strict tiering + load shedding as in Scion. Reusing REAPI
   avoids inventing a server. *Impact: high for CI and fresh boxes.
   Difficulty: M.*

4. **Early cutoff on `.rmeta` content (Nix-CA style).** Key downstream units by
   the *content digest* of upstream `.rmeta`, not by upstream's input hash. A
   private function-body edit rebuilds one crate, not the reverse-dep cone.
   Requires deterministic rmeta (check with `-Z` reproducibility flags).
   *Impact: very high for deep workspaces. Difficulty: M-L.*

5. **Watcher-driven freshness instead of stat-walks.** Run a per-worktree
   watcher (FSEvents / inotify / fanotify, or Watchman's `since` clocks) in the
   daemon, maintaining per-crate dirty bits and cached tree ids; `tinymold
   build` with no changes answers in milliseconds without running cargo's
   fingerprint walk. Use git's racily-clean rule + a binary index as the
   fallback (OpenScion's JSON state lost to git at 10k files). *Impact: medium
   (big for no-op/agent "check after every edit" loops). Difficulty: M.*

6. **Get hashes from git, not from the disk.** For files clean in `git status`
   (fsmonitor-accelerated), the content id is already in the index/tree; a
   crate directory's id at HEAD is `git rev-parse HEAD:crates/foo`. Compute
   source keys as "tree id at HEAD + overlay of dirty files". Same idea as
   EdenFS-provided digests and Bazel's xattr digests, with zero new VFS.
   Note: git uses SHA-1/SHA-256, so key on git ids directly or map to BLAKE3.
   *Impact: medium-high (keying cost ~O(dirty files)). Difficulty: S-M.*

7. **Commit-addressed prebuilt snapshots ("seeds").** CI publishes, for every
   main commit, the action-cache entries (or a manifest) for the default
   profile/targets. A new agent worktree at commit C, or at a merge base near
   C, starts warm. Pair with Scion's snapshot-boot idea for fresh machines.
   Avoid Scion's "seeds drifted from main" problem by keying on content, not
   on the commit, so near-misses still hit. *Impact: high (cold start of every
   new agent). Difficulty: S-M.*

8. **Cheap worktree creation via CoW / lazy materialization.** Create agent
   worktrees by reflink-cloning a warm `target/` (APFS/btrfs/XFS) or, longer
   term, a FUSE/NFS-backed `target/` that materializes artifacts from the CAS
   only when read (Bazel Remote Output Service model). *Impact: high for
   short-lived agent sandboxes. Difficulty: S (reflink) to L (VFS).*

9. **Build-without-the-bytes for agents.** Most agent loops need diagnostics,
   not binaries. Serve `cargo check`/clippy results (JSON diagnostics keyed by
   action) straight from the cache, and only materialize `.rlib`/binaries when
   a test or run needs them. *Impact: medium-high (check is the dominant agent
   command). Difficulty: M.*

10. **Diff-vs-merge-base affected-crate selection.** Like Scion's
    diff-without-checkout: map changed paths -> owning crates -> reverse deps
    via `cargo metadata`, and run only affected `test`/`clippy` targets; cache
    test results keyed by the test binary digest + env (Bazel-style test
    caching). *Impact: high for CI and agent verification loops.
    Difficulty: S-M.*

11. **Dedup concurrent identical work across agents (single-flight).** When
    eight agents build the same commit at once, the daemon should run each
    action once and fan the result out (in-flight dedup keyed by action
    digest), with per-unit locks instead of a target-dir lock. *Impact: high
    in swarm bursts. Difficulty: S-M once (2) exists.*

12. **Persistent incremental graph daemon (DICE-like).** Keep the resolved
    cargo unit graph, features, and fingerprints in memory across invocations;
    invalidate from watcher events; skip `cargo metadata`/resolution per
    command. *Impact: medium (hundreds of ms to seconds per command, adds up
    over thousands of agent calls). Difficulty: L.*

13. **Shareable rustc incremental state (stretch).** Make rustc's incremental
    cache relocatable and content-keyed so a fresh worktree can seed it from
    the CAS; the biggest remaining gap vs Bazel/Buck2 for edit-compile loops on
    large crates. *Impact: high for large single crates. Difficulty: XL
    (upstream rustc).*

14. **Proc-macro and build-script result caching.** Build scripts and
    proc-macro crates (uncacheable in sccache) are frequent serial bottlenecks;
    cache `build.rs` outputs keyed by declared `rerun-if-*` inputs + env, and
    cache proc-macro dylibs by content with remapped paths. *Impact: medium.
    Difficulty: M.*

15. **Observability-first rollout.** Ship a hit/miss explainer ("why did this
    unit miss: rustc flag X differs, env Y differs") per action; this is what
    made Bazel/REAPI caches tunable, and Scion relied on >99% per-tier hit
    rates as the health metric. *Impact: indirect but decisive. Difficulty: S.*

### Suggested ordering

(15) + (1) + (2) + (11) first: they turn N worktrees into roughly one build on
a single box. Then (6) + (5) for fast keys, (7) + (3) for cold starts and CI,
(4) + (10) + (9) for agent loops, and (8), (12)-(14) as larger bets.

All sources are linked inline in the section that uses them.
