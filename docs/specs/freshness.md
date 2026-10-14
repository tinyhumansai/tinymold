# Content freshness and target snapshots

- **Status:** Implemented (roadmap Phase 2)
- **Owner:** Steven Enamakel
- **Date:** 2026-10-14
- **Plan:** [`../plans/freshness.md`](../plans/freshness.md)

## Problem

Cargo decides whether a unit is stale by comparing source mtimes with the
mtime of its last build. Measured on darwin arm64
([`bench/README.md`](../../bench/README.md)), this repeats work that a
content check would skip:

- **Mtimes alone.** Touching every source file without changing a byte
  rebuilds ripgrep in 0.84 s, more than a real one-line edit (0.52 s). A
  checkout, rebase, or branch switch has the same effect.
- **A new path.** A second worktree of the same commit rebuilds everything
  (3.8 s for ripgrep, 13.2 s for bat), because nothing is shared between
  target directories.
- **A round trip.** Switching branches A→B→A rebuilds everything that
  differed between A and B twice, once in each direction, although the
  return ends exactly where it started.

## Goals and non-goals

Goals:

- Make a content-identical workspace build as a no-op, whatever happened to
  its mtimes.
- Start a new worktree, or return to a branch already built, from a target
  directory that is already warm, at a cost of milliseconds.
- Never make cargo treat an output as current when it was built from
  different content.

Non-goals:

- Caching individual units across different commits. That is the unit cache
  of roadmap Phase 3.
- Changing cargo. Everything here works by setting mtimes and cloning
  directories that cargo already understands.
- Saving disk. Snapshots trade disk for time
  ([ADR 0005](../adr/0005-trade-disk-for-time.md)).

## Behavior

### `tinymold fresh record`

Writes `<target>/.tinymold/fresh`: the mtime, size and BLAKE3 digest of every
workspace file, plus two values that describe the target directory:

- the **stamp**, the newest mtime among cargo's fingerprint files;
- the **fingerprints** digest, over every fingerprint file's path and mtime.

- **Run it after a build.** Its cost is a stat walk: about 5 ms on ripgrep's
  212 files when nothing changed.
- **What the walk skips:** `.git`, the target directory itself, any directory
  holding a `CACHEDIR.TAG` (other cargo target directories), and any nested
  directory with its own `.git` (submodules and worktrees).
- **Digest reuse.** A file whose size and mtime match the previous manifest
  reuses its digest. The exception is a file modified at or after the
  previous record, or within one timestamp tick before it, which is hashed
  again (git's racily-clean rule). The record's time is read from the
  filesystem's clock, as the wrapper does
  ([rustc-wrapper spec](rustc-wrapper.md#invariants)).
- **Skipped paths.** Paths that cannot be written as one line, because they
  are not UTF-8 or contain a newline, are skipped and counted.

### `tinymold fresh restore`

Applies the manifest to the workspace, file by file:

| Workspace file | Action |
|---|---|
| Content matches the manifest | Mtime set back to the recorded one (`restored`), unless it already matches (`unchanged`) |
| Content differs, mtime newer than the stamp | Left alone (`changed`); cargo rebuilds as usual |
| Content differs, mtime not newer than the stamp | Mtime set to now (`changed`), so cargo cannot take the outputs for current ones |
| Missing | Counted (`missing`) |

If the target directory's fingerprints digest no longer equals the
manifest's, something was built after the record. The restore then does nothing and exits 1. It also
exits 1 when there is no manifest.

### `tinymold snapshot save [NAME]`

1. Records the manifest.
2. Clones the target directory into `<store>/snapshots/<NAME>`. The name
   defaults to the workspace's git `HEAD` commit.

On APFS the clone is a single `clonefile` call, which took about 7 ms for
ripgrep's 200 MB `target/`. Elsewhere each file is reflinked or copied with
its mtime preserved. The clone is staged under a temporary name and renamed
into place, so a reader never sees a partial snapshot.

### `tinymold snapshot restore [NAME] [--replace]`

1. **Pick a snapshot.** Use `NAME` if given. Otherwise score each snapshot by
   how many of its recorded files match the workspace by content, and take
   the best one.
2. **Clear the way.** If the target directory exists and `--replace` was not
   passed, fail. With `--replace`:
   - if the old target's manifest is still valid and it can be renamed into
     the store, keep it as snapshot `auto-<stamp>`;
   - otherwise delete it.
3. **Clone** the snapshot into the target directory.
4. **Apply the manifest.** Hash every file, because equal mtimes from
   another workspace say nothing about content, then apply the table above.
5. **Refuse an unusable snapshot.** If its manifest does not apply, remove
   the restored copy and fail with `SnapshotUnusable`.

### `tinymold snapshot list`

Prints each snapshot's name and its file count.

## Invariants

1. **A restore only reproduces a state cargo has seen.** A file gets an old
   mtime back only when two things hold:
   - its content is byte-identical to what the target's outputs were built
     from;
   - the target has not been rebuilt since.

   The stamp enforces the second condition. Without it, an edit, a build and
   an undo would give the undone file an mtime older than the edited build's
   outputs, and cargo would wrongly treat those outputs as current.
2. **After a restore, cargo cannot take an output built from different
   content for a current one.** Every file whose content differs ends up
   newer than the stamp.
3. **Snapshots are immutable once saved.** Saving the same name again
   replaces the snapshot atomically.
4. **Only cargo's fingerprints describe the target.** Cargo keeps one
   fingerprint directory per unit, at `<profile>/.fingerprint/<unit>/`, or
   `<profile>/build/<pkg>/<hash>/fingerprint/` in build-dir layout v2.
   - It rewrites a unit's fingerprint after every build of that unit.
   - It deletes the fingerprint before rebuilding, so a failed build changes
     the digest too.
   - A no-op build leaves fingerprints alone, even though it rewrites
     `.cargo-lock`, `.rustc_info.json` and the uplifted `<bin>.d`.

   Reading only the fingerprints also keeps the check cheap: about 600 files
   for bat's 9,667-file `target/`, or 14 ms for a whole `fresh restore`.

## Constraints

- **Store and checkouts on one filesystem.** A cross-volume store makes saves
  and restores copies, and `--replace` deletes the old target instead of
  keeping it.
- **Snapshots keep absolute paths.** Debug info and incremental state inside
  a cloned target still name the worktree they were built in. Builds are
  correct, but a debugger may show the first worktree's source paths. Path
  remapping arrives with the unit cache in Phase 3.
- **A save right after a large build waits for writeback.** `clonefile` has
  to flush dirty data first: 3.5 s after ripgrep's full build, about 5 ms
  once the data was on disk.

## Acceptance criteria

All are measured with `bench/bench.py scenarios --modes baseline,tinymold`.

- Every-file-touched rebuild at no-op time: ≤ 0.12 s on ripgrep and bat.
- Second-worktree build, including the snapshot restore: ≤ 0.5 s on ripgrep
  and ≤ 1.5 s on bat.
- A branch switch A→B→A compiles nothing on the return to A.
- Unit tests pin the manifest encoding, and cover every restore row and
  every error variant.

### Inputs tinymold does not control

Some crates make `.git` files build inputs. bat's binary lists `.git/index`
and `.git/logs/HEAD` in its dep-info, for its version metadata:

- `git` commands rewrite those files;
- in a second worktree `.git` is a file, so `.git/logs/HEAD` does not exist.

tinymold never touches `.git`, so cargo correctly rebuilds bat's binary after
a branch switch and in every second worktree. One rebuilt unit is the floor
for such a crate.

## Open questions

- Should `fresh record` and `snapshot save` run automatically? Two options:
  - a `post-checkout` hook plus a cargo alias;
  - folding them into the rustc wrapper in Phase 3, so no hook is needed.
- Should snapshot saves run in the background after a build, to hide the
  writeback wait?
- A garbage-collection policy for `auto-*` snapshots. ADR 0005 says to keep
  them, but an explicit `tinymold snapshot gc` is likely wanted.
