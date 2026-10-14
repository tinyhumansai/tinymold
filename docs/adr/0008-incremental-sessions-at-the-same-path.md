# 8. Restore incremental sessions only at the path that made them

- **Status:** Proposed
- **Date:** 2026-10-15

## Context

On a cache hit rustc does not run, so it writes no incremental session for
that unit. A target directory filled from the store therefore holds almost
no incremental state. In the edit-sequence benchmark (`bench.py edits`, the
tinymold repository, darwin arm64), a fresh target directory left 18 MB under
`incremental/` against plain cargo's 174 MB. The first private-body edit
after it took 0.76 s, where plain cargo took 0.37 s. The cache made the
next edit colder than having no cache at all.

The obvious fix is to store each incremental unit's session directory with
its action and to restore it on a hit. Two questions decide whether that
works, both answered by a spike on ripgrep 14.1.1 (`rg` crate, private-body
edit, darwin arm64, under load from a concurrent benchmark):

| After a fresh target directory filled from the store | Edit rebuild |
|---|---:|
| No session (today) | 2.50–3.29 s |
| The session saved after the cold build, restored at the same path | 0.99–1.66 s |
| That session restored into a second checkout at another path | 2.20 s (cold: 1.66 s) |

- **At the same path, rustc reuses a restored session.** It validates the
  session against the upstream crates, which the store restores
  byte-identical, so the edit regenerates only the changed codegen units.
- **At another path, it does not.** The working directory is part of what
  rustc's incremental state is checked against, so a session from another
  worktree is discarded and the compile runs cold. cargo gave ripgrep's
  units the same metadata hash in both checkouts, so the session directory
  names matched; the rejection came from rustc itself.

## Decision

Store a unit's incremental session with the action of a miss, and restore it
on a hit, only into the same incremental directory path it was taken from.

- The session is **not** part of the key. The key keeps the bare
  `-C incremental` flag, as today.
- **Correctness stays with rustc.** A session is a cache of rustc's own,
  which rustc validates before using any of it. A wrong or stale session
  costs reuse, never a wrong output. Restoring one never fails a hit: a
  missing or broken session only leaves the next edit cold.
- **Sessions are restored by reflink or copy, never by hardlink**, because
  rustc rewrites the files of a session it continues.
- **Most of a session is already in the store.** Its object files are the
  objects the action stores anyway, so they deduplicate. What is new per
  generation is the dep graph and query cache, a few megabytes per crate.
  `gc --keep-generations` bounds those like every other output.

Not decided here: sharing sessions across worktrees, which would need rustc
to accept a session made in another working directory.

## Consequences

- The first edit after `cargo clean`, a deleted target directory, or a CI
  runner at a fixed path with incremental on is about as fast as an edit in a
  directory that was never emptied.
- Each incremental action grows by its session's unique files. The edit
  benchmark has to show that cost next to the latency gained before this is
  accepted.
- New worktrees still start with cold sessions. They get their units from
  the store, so only their first edit of each crate is slower.
