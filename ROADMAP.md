# Roadmap

The phased plan lives in [`docs/plans/roadmap.md`](docs/plans/roadmap.md), next
to the specs it delivers. In short:

| Phase | Goal |
| --- | --- |
| 0. Foundations | BLAKE3 digests, the local content-addressed store, `tinymold doctor`, the research and design docs, and 0.0.1 on crates.io |
| 1. Measure | Diagnose where a build's time and bytes go, and a benchmark harness that can prove a speed-up |
| 2. Cache core | A rustc wrapper with path-normalized keys, so no unit compiles twice on one machine |
| 3. Hot server | A per-machine daemon: stat cache, file watching, single-flight across worktrees, tiered storage with quotas |
| 4. Linking | Fastest linker per platform, link-skip, one link for all tests, a shared dev dylib for dependencies |
| 5. Early cutoff and remote | Skip dependents when a crate's interface is unchanged; a LAN/remote tier over the Remote Execution API |
| 6. Research bets | Relink-don't-rebuild, incremental linking with wild, proc-macro expansion caching |

## Out of scope

- Writing a new linker from scratch: tinymold contributes to wild and mold
  instead.
- Forking rustc or replacing cargo: tinymold wraps them.
- Anything that trades correctness for speed: when a cache cannot prove a hit
  is right, it misses.
