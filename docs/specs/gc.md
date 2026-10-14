# Last-use tracking and `tinymold gc`

- **Status:** Implemented
- **Owner:** Steven Enamakel
- **Date:** 2026-10-14
- **Plan:** [`../plans/remote-and-ci.md`](../plans/remote-and-ci.md)
- **Builds on:** [ADR 0005](../adr/0005-trade-disk-for-time.md),
  [`rustc-wrapper.md`](rustc-wrapper.md)

## Problem

The store only grows. Every compile with new inputs adds an action and its
objects, and nothing removes them.

- **Developer machines.** Disk is cheap (ADR 0005), but not unlimited.
- **CI.** The stateless GitHub backend saves the whole store as one cache
  archive. Its size is bounded by the CI cache (10 GB per repository on
  GitHub), and every job pays to download it.

ADR 0005 promises an explicit `tinymold gc` and a store-size report, rather
than a budget enforced on the hot path. Neither exists yet.

What to keep should depend on use. An entry that builds keep hitting is
worth its bytes. An entry nothing has asked for in weeks belongs to code
that no longer exists.

## Goals and non-goals

Goals:

- Record when each action was last used, at the cost of one metadata write
  per hit.
- Remove entries unused for longer than a cutoff, and optionally enforce a
  size budget by evicting the least recently used entries first.
- Never leave the store in a state that serves a wrong hit, even while
  wrappers run concurrently.

Non-goals:

- **Eviction on the hot path.** GC is an explicit command.
- **Use counts.** Within a cutoff window, "used recently" captures what
  matters, and a per-hit counter would add a read-modify-write to every hit.
- **Remote expiry.** The remote server runs its own LRU
  ([ADR 0006](../adr/0006-remote-tier-reapi-bazel-remote.md)).

## Behavior

### Last use

- **The last-use time is a record file's mtime.** It applies to the action
  (`entries/<crate>/<key>`) and to its pre-key record (`prekeys/<pre>`).
- **Writing the record sets it**, and **every hit sets it to now**,
  including hits filled from the remote tier. The touch is best effort: a
  failure is ignored, and never fails the hit.
- Objects carry no last-use time of their own. An object lives as long as a
  live action references it.
- Archivers such as `tar` (and so `actions/cache`) preserve mtimes, so last
  use survives a CI cache round trip.

### `tinymold gc`

```text
tinymold gc [--max-age <age>] [--max-size <size>] [--snapshots-max-age <age>] [--dry-run]
```

- `<age>` is a whole number with a unit: `s`, `m`, `h` or `d` (`14d`, `36h`).
- `<size>` is a whole number with an optional `K`, `M`, `G` or `T` suffix
  (powers of 1024), or plain bytes.
- With no limits given, GC only removes garbage: objects no action
  references, and stale temporary files.

It runs these steps in order:

1. **Expire actions** whose last use is older than `--max-age`.
2. **Enforce `--max-size`.** Let the store size be the bytes of every object
   a surviving action references. While that exceeds the budget, evict the
   least recently used action. An object's bytes are freed only when no
   surviving action still references it.
3. **Expire pre-key records** older than `--max-age`, and digest-cache files
   (`stat/`, `rustc/`) older than `--max-age`. These are caches: losing one
   costs a rehash, never a wrong hit.
4. **Sweep objects** that no surviving action references and that are older
   than the grace period (one hour). Then remove staging files in `tmp/` and
   record temporaries that are older than the grace period, and lock files
   in `locks/` older than the grace period that no wrapper holds
   ([`rustc-wrapper.md`](rustc-wrapper.md)).
5. **Expire snapshots** whose directory is older than `--snapshots-max-age`.

It prints counts and bytes removed and kept. `--dry-run` reports the same
numbers and deletes nothing.

`tinymold doctor` reports the store's size: its object count and bytes.

### Errors

| Situation | Result |
|---|---|
| `--max-age`, `--max-size` or `--snapshots-max-age` unparsable | Exit 2, `invalid duration` / `invalid size` |
| A corrupt action record | Treated as unreferencing; the record is removed |
| A file vanishes during the walk | Skipped (another GC or wrapper removed it) |
| Any other I/O error | Exit 1 with the path |

## Invariants

- **No wrong hits.** GC only deletes. An action whose object was swept gives
  a failed restore, and the wrapper then treats it as a miss and recompiles
  (`rustc-wrapper.md`). The miss re-stores the objects and repairs the
  entry.
- **The grace period protects ingest in progress.** A wrapper inserts
  objects before it writes their action, so an object younger than the grace
  period is never swept for being unreferenced. Storing an object that is
  already present refreshes its mtime, so an old object a new action is
  about to reuse counts as young too.
- **Policy stays out of the CAS.** `tinymold-cas` gains listing and removal
  primitives only. Which objects to remove is decided in `tinymold`.

## Acceptance criteria

- A hit moves its action's mtime forward. A miss leaves other actions alone.
- `gc --max-age 1d` removes an action last used two days ago and keeps one
  used today. An object both reference survives until neither does.
- `gc --max-size` keeps the most recently used actions that fit.
- A fresh unreferenced object (younger than the grace period) survives GC.
- `--dry-run` changes nothing on disk.
- After GC, a build of retained work still hits.

## Open questions

- Whether the hot server should run GC on a schedule once it exists.
