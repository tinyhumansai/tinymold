# The rustc wrapper: a unit cache keyed by content

- **Status:** Accepted (roadmap Phase 3, first slice)
- **Owner:** Steven Enamakel
- **Date:** 2026-10-14
- **Builds on:** [`cache-key.md`](cache-key.md),
  [ADR 0003](../adr/0003-two-level-content-addressed-cache.md),
  [ADR 0004](../adr/0004-store-layout-aligned-with-cargo.md),
  [ADR 0005](../adr/0005-trade-disk-for-time.md)

## Problem

Phase 2 snapshots warm whole target directories, but they help only when a
whole commit matches. These cases still compile units that were compiled
before:

- a branch whose crates were built on another branch;
- `cargo test` after `cargo build`;
- a clean build of a commit whose dependencies have not changed.

sccache caches some of these units, but it refuses incremental units and
pays for its own process. With every compile a hit, it still left a second
bat worktree at 4.3 s.

## Goals and non-goals

Goals:

- **Never run rustc for an invocation already run with the same inputs**, in
  any worktree, on this machine.
  - This includes binaries, tests, proc macros, build-script binaries, and
    incremental workspace units.
  - It excludes build-script *runs*, which cargo does not route through
    `RUSTC_WRAPPER`.
- **Make a hit cheap.** A hit restores outputs by reflink and replays rustc's
  output. It does not copy bytes.
- **Never break a build.** Any internal failure falls back to running rustc.

Non-goals for this slice:

- the daemon (roadmap Phase 6);
- a remote tier;
- build-script output caching;
- verify mode;
- early cutoff tuning.

## How it is used

```sh
RUSTC_WRAPPER=tinymold cargo build
```

Cargo runs `tinymold <rustc> <args…>`. The binary treats an invocation as a
wrapper call when its first argument is not a tinymold subcommand and names a
program: a path containing a separator, or a file named `rustc` or
`clippy-driver`.

- **Cacheable:** only invocations whose program is `rustc` and which compile
  a crate.
- **Run unchanged:** everything else, including `-vV`, `--print`, stdin
  input, and `clippy-driver`.

## The key

**The pre-key** is known before rustc runs. It is the BLAKE3 digest of these
fields, in order, each length-prefixed (`tinymold_hash::Hasher`):

1. **Schema tag:** `tinymold/rustc/v3`.
2. **The compiler:** `rustc -vV` output. It is cached per rustc path, size
   and mtime, so it runs once per toolchain.
3. **The arguments, normalized in order:**
   - **Dropped, because they say where outputs go rather than what they
     are:** `--out-dir`, `-C incremental=`, and `-L dependency=`.
   - **Dropped when every `--cap-lints` says `allow`:** lint-level flags,
     `-A`, `-W`, `-D`, `-F` and their long forms. Cargo caps every non-path
     dependency this way, and rustc then ignores them, so
     `RUSTFLAGS="-D warnings"` does not re-key dependencies. `--force-warn`
     is not capped and stays
     ([test-cache spec](test-cache.md#flag-scoping)).
   - **Each `--extern name=path` becomes `name=<digest of the file>`.**
     That covers `.rlib`, `.rmeta` and proc-macro dylibs alike, so an
     upstream rebuild that produces identical bytes keeps the dependent's
     key (early cutoff).
   - **Each `-L native=|all=|framework=|crate=` directory becomes the
     digests of the files in it.** A static archive (`!<arch>`) is hashed
     with each member's time, uid and gid fields zeroed.
     - `ar` stamps those fields on every build, and the linker never reads
       them.
     - Measured on bat: its `libgit2.a` was member-for-member identical
       across two clean builds once they were zeroed.
     - Without this, every rebuilt `-sys` crate would change its dependents'
       keys.
   - **Every other path is rewritten:** prefixes of the current directory
     become `${CWD}`, the profile directory becomes `${PROFILE}`, and
     `CARGO_HOME` becomes `${CARGO_HOME}`.
4. **Compiler environment:** the values of `RUSTC_BOOTSTRAP`,
   `MACOSX_DEPLOYMENT_TARGET`, `SDKROOT` and `SOURCE_DATE_EPOCH`.
5. **The linker, for linking units** (bin, test, dylib, cdylib, proc-macro).
   The linker is `-C linker=` or `cc`, resolved on `PATH`. The key gets its
   path, size and mtime.

**The full key** adds what rustc actually read, taken from the dep-info of an
earlier compile with this pre-key (two-phase lookup, as in `cache-key.md`):

- every input file, as its normalized path plus its content digest;
- every `# env-dep:` variable, as its raw current value or "unset".

Env values are **not** normalized. A crate that uses
`env!("CARGO_MANIFEST_DIR")` bakes an absolute path into its output. It must
miss in another worktree, and it does.

## Outputs, hits and misses

**On a miss**, tinymold:

1. runs rustc, streaming its stdout and stderr through line by line, so that
   cargo's pipelining still sees the `artifact` notifications on time;
2. if rustc succeeded, and no input changed during the compile, stores the
   unit's outputs: every file in `--out-dir` whose name starts with
   `<crate><extra-filename>` or `lib<crate><extra-filename>`.
   - **The mtime is not checked.** Incremental rustc hardlinks artifacts it
     reuses (objects, sometimes the `.rmeta`) back into the out-dir with
     their old mtimes. An earlier version that filtered by mtime stored
     actions without them. That left a stale `.rmeta` next to a restored
     `.rlib`, and the final link combined a crate with dependents compiled
     against another version of it.
   - **The outputs the `--emit` kinds imply must be present:** `.d`,
     `.rmeta`, and a library's `.rlib`.
3. stores the outputs in the CAS, spread over up to eight threads, then
   writes the ActionResult (`entries/<crate>/<key>`, per ADR 0004), and
   records the input set under the pre-key.

**On a hit**, tinymold:

1. keeps every output already in the out-dir that the last hit or miss of
   this unit there placed, unchanged since and with the digest the action
   wants (see below);
2. removes every other file in the out-dir with the unit's prefix, then
   places the remaining outputs by reflink, falling back to copy, and never by
   hardlink, because linkers may rewrite outputs in place;
3. sets each output's mtime to now, as if rustc had just written it, except
   for object files kept in place: rustc itself leaves the objects it reuses
   with their old mtimes, and nothing compares them;
4. replays stdout and stderr, then exits 0.

Steps 2 and 3 run per output, in turn for a unit with fewer than 16
outputs and spread over up to eight threads above that, since an
incremental compile with split debuginfo leaves hundreds of object files.
Either way, stdout and stderr are read before any file is removed, so a
missing replay object fails the hit with the out-dir untouched, and any
output that cannot be placed fails the whole hit, which then runs rustc.

**Outputs kept in place.** After every hit and every stored miss, the
wrapper records the identity (device, inode, size, mtime) and digest of each
binary output it left, one record per unit and out-dir, under `stat/`. An
output is kept when its identity still matches that record. As with the stat
cache's entries for outputs, identity alone is trusted, because these are
files whose writer has finished and which nothing rewrites without going
through the wrapper again. A replaced file gets a new inode or mtime. Without
this, a hit after early cutoff spent nearly all its time placing again the
hundreds of objects already there: 63 ms for ripgrep's `grep_printer` and
151 ms for `rg`, against under 1 ms for the lookup. A missing or unreadable
record only means every output is placed again.

**Text gets placeholders.** Dep-info, stdout and stderr name absolute paths,
so they are stored with the out-dir, profile, current-directory and
`CARGO_HOME` prefixes replaced by placeholders, and expanded on restore.
Otherwise cargo would read the first worktree's paths and check the wrong
files' mtimes. Binary outputs are restored as stored.

**Concurrent builds of one unit compile it once.** Eight agents building one
commit in eight worktrees ask for the same unit at about the same time, and
each would miss and run rustc. After a local miss, and before the remote tier
or rustc, the wrapper takes an exclusive lock (`flock`, or `LockFileEx` on
Windows) on `locks/<key>` in the store. The key is the full key the newest
recorded input set gives for today's files, the compile this one most likely
is, or the pre-key when no input set is recorded yet. Locking on that full key
rather than the pre-key keeps compiles with different sources, such as
worktrees with different edits, from waiting on each other.

- **Free:** the wrapper holds the lock until the compile is stored, so others
  wait for it.
- **Held:** it waits, polling, then looks the unit up again. A hit is
  restored. A miss (the other compile failed, could not be stored, or was a
  different compile) runs rustc while holding the lock.
- **Bounded:** a wrapper waits at most ten minutes, then compiles without the
  lock. A holder that dies releases its lock with its process.
- **Never required:** a lock that cannot be opened or taken, such as on a
  filesystem without locking, is skipped. Locking only saves work; a hit
  never depends on it, and the log records how long a wrapper waited.

This is Zig's build-cache locking
([`docs/research/zig.md`](../research/zig.md)) without its shared lock for
hits: a hit takes no lock, and GC already protects an ingest in progress
([`gc.md`](gc.md)). A waiting wrapper keeps the cargo job slot it was started
with, and its dependents start only when the whole unit has been stored,
not at the earlier `.rmeta`, so waiting trades some pipelining for not
running the same compile twice.

**What is never cached:**

- failed compiles;
- units whose outputs include a directory;
- units whose inputs changed mid-compile;
- units whose inputs lie outside every known root and are not regular files.

## Invariants

- **After a hit, the out-dir holds exactly the stored outputs for the
  unit.** Nothing from another compile of the unit remains beside them.
- **A hit is only served when every input matches.** Every input file named
  by the recorded dep-info matches by content, and every recorded env-dep
  value matches. Absolute workspace paths never enter the key except through
  env values, which then make the key path-specific, and for compiles
  instrumented with `-C instrument-coverage`, whose coverage mappings embed
  the workspace path, so their pre-key includes the current directory.
- **The store is never written in place.** Restored files are private
  reflinks or copies.
- **Nothing is half-visible.** The ActionResult is written after all of its
  objects, through a rename, and pre-key records are written the same way.
- **Digests may come from the stat cache.** An `--extern` file's digest is
  reused from `stat/` when its size, mtime and inode are unchanged and its
  mtime was written before the cache entry (git's racily-clean rule).
- **Mtimes are compared in the filesystem's clock and tick.** "Now" is the
  mtime of a probe file the wrapper writes in `stat/` once per call, not the
  wall clock, which can run ahead of the coarse clock Linux stamps files
  with. A file counts as written before that instant only if its mtime plus
  one tick is not after it, where the tick is inferred from the mtime's
  trailing decimal zeros: a whole second counts as two (FAT), so a file
  rewritten within its second on HFS+ or ext3 is not trusted. The same rule
  decides whether an input changed during the compile, and the probe's
  reading is the time an input set is recorded as of.

## Acceptance criteria

Measured with `bench/bench.py scenarios --modes baseline,sccache,tinymold`,
where tinymold mode also sets `RUSTC_WRAPPER=tinymold`:

- Filling an empty cache costs ≤ 10% over an uncached clean build.
- A warm clean build beats sccache's on ripgrep and bat.
- Unit hits are reported, as sccache's are.
- On the branch-switch and test-build cases, units compiled before are
  served as hits.

## Open questions

- Verify mode: rebuild on a sample of hits and compare. Outputs of
  incremental compiles are not byte-reproducible, so the comparison must be
  semantic or limited to non-incremental units.
- Moving ingest off cargo's critical path, through a background thread that
  outlives rustc or through the daemon.
- Caching build-script runs, which needs a hook cargo does not provide.
