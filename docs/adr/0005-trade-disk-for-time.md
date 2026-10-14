# 5. Trade disk for time

- **Status:** Accepted
- **Date:** 2026-10-14

## Context

tinymold's first design treated `target/` footprint as a first-class goal. It
planned tiered storage, zstd cold tiers, eviction budgets and per-project
quotas ([hashing §4.5](../research/hashing-and-cache-server.md#45-size-budget-proposal)).

On the machines tinymold targets, NVMe space is cheap and plentiful. Developer
and agent wall time is not. The committed baselines put the cost in seconds
spent repeating work, not in bytes
([research §9](../research/compilers-2026.md#9-what-tinymolds-baselines-say)):

- a second worktree rebuilds everything;
- test builds repeat links;
- a touched but unchanged file triggers a rebuild.

The dedup probe shows that chunking objects would add only about 13 points of
sharing over whole files.

## Decision

When saving time costs disk, tinymold spends the disk:

- keep whole `target/` snapshots per branch head, restored by `clonefile` or
  reflink;
- keep per-key incremental directories, cached link outputs and test
  binaries;
- allow speculative prebuilds, such as `cargo build --tests` after a
  successful `cargo build`;
- store objects uncompressed on the local tier, so no hit pays for
  decompression.

The following are deferred until disk becomes the binding constraint:

- chunked storage (rlib members, DWARF sections);
- zstd cold tiers;
- eviction budgets.

Garbage collection still exists, but it is an explicit command, not a budget
enforced on the hot path.

## Consequences

- Hits and restores stay O(1) in bytes copied. Ingest never compresses.
- Store size grows with history. `tinymold doctor` reports it, and
  `tinymold gc` removes entries that are unreferenced or older than a cutoff.
- Benchmarks gate on wall time. `target/` and store sizes are still recorded,
  but they are not pass/fail criteria.
- The footprint ideas in the research catalogue remain valid. They are
  re-prioritized, not rejected.
