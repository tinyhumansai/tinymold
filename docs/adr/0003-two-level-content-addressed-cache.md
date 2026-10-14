# 3. Two-level content-addressed cache

- **Status:** Accepted
- **Date:** 2026-10-14

## Context

tinymold caches the outputs of rustc and linker invocations so they never run
twice for the same inputs. There are two common shapes for such a cache:

- **One level, key to bytes.** sccache and ccache map a hash of the
  invocation directly to stored output files. Simple, but identical outputs
  produced by different invocations are stored twice, there is no cheap way
  to ask "is this exact file already stored", and downstream keys can only
  fold in upstream *keys*, so any upstream change cascades.
- **Two levels, key to manifest to blobs.** Go's build cache maps an action
  ID to an output ID; REAPI maps an Action digest to an ActionResult that
  lists output digests, and stores the bytes in a separate content-addressed
  store (CAS). Bazel, Buck2, BuildBuddy and NativeLink all use this shape
  ([research: Go](../research/fast-tools.md#52-build-cache-design-the-model-to-copy),
  [research: REAPI](../research/source-control-and-caching.md#34-remote-execution-api-rev2-buildbuddy-nativelink-others)).

Three tinymold goals depend on the choice:

1. **Dedup across worktrees and projects.** 142 of 144 same-named rlibs in two
   unrelated projects were byte-identical [measured], and N worktrees of one
   project should cost about 1x their dependencies on disk.
2. **Early cutoff.** A dependent should be keyed by the *content* of its
   upstream `.rmeta`, so an edit that leaves that file byte-identical stops
   propagating. This needs output digests to exist as first-class values.
3. **A remote tier without a custom protocol.** REAPI servers already
   implement the two-level model.

## Decision

The cache has two levels:

- **ActionCache:** `ActionKey → ActionResult`. The ActionKey is a BLAKE3-256
  digest of every input to one invocation ([cache-key spec](../specs/cache-key.md)).
  The ActionResult is a small manifest: for each output, its kind, path
  relative to the target dir, digest, size and executable bit, plus the
  diagnostics digests, exit code and the recorded input set.
- **CAS:** `Digest → bytes`, immutable and read-only, laid out as
  `objects/<2 hex>/<62 hex>`. Implemented locally by `tinymold-cas`.

Rules that make it safe:

- Blobs are inserted by writing a temp file, verifying its digest and
  renaming it into place. The ActionResult is written only after every blob it
  names is present, so any manifest a reader finds is complete.
- Downstream keys include upstream **output digests** (rmeta for compiling
  actions, rlib for linking actions), never upstream ActionKeys.
- Eviction removes a blob only when no retained ActionResult references it.
- The same model is used at every tier (local warm and cold, remote REAPI),
  so a lookup can move between tiers without translation.

## Consequences

- Each distinct output is stored once regardless of how many actions,
  worktrees or projects produced it.
- Early cutoff and link-skip become key computations rather than special
  cases.
- REAPI servers (bazel-remote, NativeLink, BuildBuddy) can serve as the remote
  tier unmodified; `FindMissingBlobs` uploads only what the server lacks.
- A hit costs two lookups (manifest, then blobs) instead of one. Manifests are
  small and indexed, so this is negligible next to materialization.
- Garbage collection must track references from manifests to blobs, which is
  more work than deleting whole entries.
- Outputs that are not reproducible (incremental sessions, impure build
  scripts) produce different blobs for one key. Verification sampling and the
  `verified` flag on each ActionResult exist to detect and contain this.
