# Specifications

Specifications define what the system must do before implementation details
take over. Create one for behavior that changes a public API, crosses module
boundaries, introduces a durable data format, or has meaningful operational
constraints.

Use a short kebab-case filename such as `retry-policy.md`. Each specification
should contain:

1. **Status and owner** — Draft, Accepted, Implemented, or Superseded.
2. **Problem** — the user or system need, without prescribing a solution.
3. **Goals and non-goals** — the exact boundary of the work.
4. **Proposed behavior** — public API, inputs, outputs, errors, and examples.
5. **Invariants and constraints** — properties every implementation must keep.
6. **Acceptance criteria** — externally observable pass/fail conditions.
7. **Open questions** — unresolved decisions that block acceptance.

After the specification is accepted, create a linked implementation plan in
[`../plans/`](../plans/README.md). Keep code snippets small enough to clarify
the contract; production code still belongs under `src/`.

## Index

| Spec | Status | Covers |
|---|---|---|
| [`architecture.md`](architecture.md) | Draft | Target architecture: components, data flow, two-level cache, storage tiers, correctness, platform notes |
| [`cache-key.md`](cache-key.md) | Draft | The rustc ActionKey and ActionResult schema, two-phase lookup, and what breaks reuse |
| [`rustc-wrapper.md`](rustc-wrapper.md) | Accepted | `RUSTC_WRAPPER=tinymold`: the unit cache's key, lookup, hits, misses and invariants |
| [`freshness.md`](freshness.md) | Implemented | `tinymold fresh` and `tinymold snapshot`: undoing mtime-only changes and warm `target/` snapshots for worktrees and branch switches |
| [`gc.md`](gc.md) | Implemented | Last-use tracking and `tinymold gc`: expiry by age, an LRU size budget, and safe sweeps |
| [`remote.md`](remote.md) | Implemented | The remote tier: REAPI v2 against bazel-remote, lookups on local miss, batch push |
| [`test-cache.md`](test-cache.md) | Draft | Affected work: which tests an edit reaches, the test result cache behind `tinymold run-test`, and flag scoping across check, clippy, test and coverage |
| [`edit-rebuild.md`](edit-rebuild.md) | Accepted | The edit loop: carried-over outputs, cached clippy, bounded store generations, incremental sessions, and the edit-sequence benchmark |
| [`cache-server.md`](cache-server.md) | Accepted | `tinymold serve`: one store behind REAPI, Bazel HTTP, Turbo, Nx, Gradle and native front-ends, plus the Go adapter |
| [`github-actions.md`](github-actions.md) | Implemented | The setup and save actions: the stateless GitHub-cache backend, the remote backend, coverage |
