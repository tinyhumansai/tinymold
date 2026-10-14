# 6. The remote tier speaks REAPI v2 to an off-the-shelf server

- **Status:** Accepted
- **Date:** 2026-10-14

## Context

CI runners start with an empty store. A GitHub-hosted runner is a fresh VM
per job, so nothing on its disk outlives the job, and a per-user daemon (the
hot server of roadmap Phase 6) cannot help it across jobs. Builds there
repeat work that another runner, or the same runner yesterday, already did.

There are two ways to carry a store between such runners:

- **No server.** Round-trip the store through the CI system's own cache,
  such as `actions/cache`. This costs nothing to run, but every job downloads
  the whole archive, and the archive is limited to what the CI cache allows
  (10 GB per repository on GitHub).
- **A shared server** that every runner and developer machine reaches over
  the network with credentials, fetching only the entries a build needs.

`architecture.md` already names REAPI v2 as the remote protocol, and
[ADR 0002](0002-blake3-for-content-digests.md) already says how a
SHA-256-only peer is served. We could write our own server, or reuse one of
the existing REAPI caches (bazel-remote, buildbarn, BuildBuddy, NativeLink).

## Decision

- **Both transports are supported.** The GitHub Action defaults to the
  stateless one (the store as a single `actions/cache` archive), and adds the
  remote tier when it is given a server URL. See
  [`../specs/github-actions.md`](../specs/github-actions.md).
- **The remote tier is an REAPI v2 client**, `tinymold-remote`, using only
  the cache services: the ActionCache, the ContentAddressableStorage, and
  ByteStream. It never asks a server to execute anything.
- **The reference server is bazel-remote**, run from its published Docker
  image on a local persistent disk. tinymold does not ship a server of its
  own. bazel-remote already provides:
  - an LRU bounded by `--max_size`, ordered by access;
  - basic auth through `--htpasswd_file`, and optional unauthenticated
    reads;
  - TLS;
  - validation of each ActionResult and of the blobs it references.
- **bazel-remote only supports SHA-256**, as its README states ("the key is
  the lowercase SHA256 hash of the entry"). So the client uses ADR 0002's
  translation layer:
  - blobs are uploaded and fetched under their SHA-256;
  - ActionCache keys are tinymold's own BLAKE3 keys, sent as opaque 32-byte
    hashes;
  - every fetched blob is re-hashed with BLAKE3 when it enters the local
    store, and checked against the digest in the tinymold record.

  Local structures never hold a SHA-256.
- **The client is written by hand**, not generated. That covers the dozen
  REAPI messages tinymold uses, as `prost` derives, plus the unary and
  streaming calls, through `tonic`'s generic client. No `protoc` and no
  build script are needed.

## Consequences

- Any REAPI cache works, not just bazel-remote. Field numbers are pinned in
  unit tests against the upstream protos.
- tonic, prost and tokio enter the dependency graph, but only in
  `tinymold-remote`. `tinymold-wrap` reaches the remote through a small
  synchronous trait, so the hot path stays free of async code when no
  remote is configured.
- **Server-side expiry is the server's LRU.** Entries that builds use often
  stay, and entries no build touches age out. Local expiry is
  `tinymold gc` ([`../specs/gc.md`](../specs/gc.md)).
- Each wrapper process opens its own connection on its first local miss.
  That costs a TLS handshake per locally-missing unit until the hot server
  exists to hold connections open.
