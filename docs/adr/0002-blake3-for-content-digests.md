# 2. BLAKE3-256 for content digests

- **Status:** Accepted
- **Date:** 2026-10-14

## Context

tinymold names every cached artifact and every cache key by a digest. Those
digests are persisted on disk, shared between worktrees and users on one
machine, and later shared with other machines through a remote tier. Once
written, a digest format is effectively permanent: changing it invalidates
every cache and every remote entry.

The requirements differ by use
([research: which hash where](../research/hashing-and-cache-server.md#12-which-hash-where)):

- Anything persisted or shared must resist deliberately constructed
  collisions, because a collision in a shared cache is a cache-poisoning
  attack, and must be stable across versions and platforms.
- Throughput matters for large artifacts (rlibs of hundreds of MB, test
  binaries), but hashing is not the bottleneck: a 4.6 GB `target/` hashes in
  about 2.4 s even with single-thread SHA-256 [measured]. Avoiding reads
  matters more than hash speed.
- The remote tier will speak REAPI v2, which allows SHA-256 and, in newer
  revisions, BLAKE3. Bazel 6.4+ and BuildBuddy accept BLAKE3.

Candidates considered:

- **SHA-256.** Universal and hardware-accelerated on ARMv8 and recent x86
  (about 1.9 GB/s wall single-thread on Apple M5 Pro, [measured]). Not
  parallelizable within one input.
- **BLAKE3.** Cryptographic, 256-bit, a Merkle tree internally, so large
  inputs hash in parallel across SIMD lanes and threads; peak single-thread
  about 12x SHA-256 on x86 with AVX-512 [vendor]. Below ~4 KB, SHA-NI can be
  faster on x86, and Bazel's Java port measured slower than SHA-256 on an M3
  Max because Java cannot use NEON; the native Rust crate has a NEON path.
  Already used by rustc's `-Zchecksum-hash-algorithm=blake3`, by OpenScion's
  object ids, and by comparable Rust caches (zccache, kache).
- **XXH3, rapidhash, gxhash.** Much faster, but not collision-resistant, and
  some are unstable across versions. Unsuitable for anything shared.

## Decision

- Every persisted or shared digest in tinymold is **BLAKE3 with a 256-bit
  output**, represented by the single `Digest` type in `tinymold-hash` and
  encoded as 64 lowercase hex characters in text form.
- No other digest type appears in any persisted or shared structure. Mixing
  types creates the keyspace confusion REAPI needs a `digest_function` field
  to manage.
- Large inputs may be hashed with BLAKE3's multithreaded or mmap modes; the
  digest is identical either way.
- Non-cryptographic hashes are allowed only where nothing is persisted or
  shared: inside the server's stat cache as a change hint, and as the hasher
  for in-memory maps (whose keys are usually digests already, so the first 8
  bytes suffice).
- A REAPI peer that only supports SHA-256 is served through a translation
  layer in `tinymold-remote`, not by adding SHA-256 digests to local
  structures.

## Consequences

- One digest type flows through hashing, the CAS, keys and the remote tier,
  so there is no conversion and no ambiguity about what a digest means.
- Digests from rustc's own checksum output (nightly) can be reused directly
  as key inputs instead of re-reading files.
- Hashing is parallel for the few large artifacts that dominate bytes.
- Git blob ids (SHA-1 or SHA-256) cannot be used as-is; a fast path that
  trusts git for clean files must map each blob id to its BLAKE3 digest once
  and memoize it.
- Performance on Apple Silicon must be confirmed with the native crate; if it
  turned out materially slower than hardware SHA-256 there, this ADR would be
  revisited by a superseding one, not edited.
- Changing the digest later means a new schema tag and a cold cache. That is
  the cost this decision accepts in exchange for a stable format now.
