# 9. tinymold is a cache server with protocol front-ends

- **Status:** Accepted
- **Date:** 2026-10-15
- **Supersedes:** [ADR 0006](0006-remote-tier-reapi-bazel-remote.md), in its
  decision that tinymold ships no server of its own

## Context

tinymold began as a Rust build accelerator with four pillars: caching,
linking, a hot cache server and hashing. Two things have since become clear.

- **Most of the measured wins came from caching.** The unit cache, freshness,
  snapshots and the test cache are the shipped results. The edit-loop
  experiments found no consistent gain from debuginfo levels, `-Zthreads` or
  Cranelift ([`edit-loop.md`](../plans/edit-loop.md)). The linker work was
  scoped to configuring existing linkers, since a stateful mold fork was
  rejected.
- **The server side is not language-specific.** A remote build cache is a
  content-addressed store plus a map from keys to results. Every ecosystem
  computes its own keys in its own client: Turborepo and Nx hash tasks,
  Gradle hashes task inputs, ccache hashes preprocessed sources, Bazel and
  Buck2 hash actions, and Go hashes actions in `cmd/go`. Each one then talks
  to a store over a small protocol. Today each protocol has its own server
  (bazel-remote, turborepo-remote-cache, Gradle's build cache node, and so
  on), and a polyglot organisation runs several of them, each with its own
  disk, eviction and credentials.

ADR 0006 chose bazel-remote as the remote tier and decided tinymold would not
ship a server. That kept Phase 3 small. But it leaves the hot server of
roadmap Phase 6 as a second, Rust-only server beside it, and every other
language on yet another server.

## Decision

tinymold becomes **one fast content-addressed cache server, with adapters**.

- **The core** is the store and the server. It holds bytes by digest,
  results by key, last-use times and an eviction budget, and it serves them
  quickly. It knows nothing about any language.
- **Front-ends** are thin protocol adapters inside the server. Each one
  translates one wire protocol onto the core: REAPI v2 (gRPC), Bazel's HTTP
  cache (`/ac/`, `/cas/`, also used by ccache), Turborepo, Nx and Gradle.
- **Clients** are adapters outside the server that hook a toolchain and
  compute its keys. The Rust adapter is the existing rustc wrapper and test
  cache (`tinymold-key`, `tinymold-wrap`). A Go adapter (`GOCACHEPROG`)
  follows. A toolchain that already has a client (Turbo, Nx, Gradle, ccache,
  Bazel, Buck2) needs only a front-end.
- **Keys stay the client's responsibility.** The server never computes a
  key for a front-end and never claims a hit is correct. It guarantees only
  that it returns exactly the bytes that were stored under a key, to a
  caller allowed to read them.
- **tinymold ships its own server**, `tinymold serve`, and it replaces
  bazel-remote as the reference remote. The Rust adapter keeps talking REAPI,
  so bazel-remote and other REAPI caches keep working as remotes.
- **Linking leaves scope.** Linker autoselect, the link concurrency governor,
  test-binary consolidation, dylib dev mode and prelinking are no longer
  planned. Linked binaries are still cached as rustc outputs by the Rust
  adapter. Edit-loop experiments on rustc itself (Phase 5) and the
  rustc-specific research bets (Phase 7) stop as well; their recorded results
  stay.

The design is in [`../specs/cache-server.md`](../specs/cache-server.md) and
the order of work in [`../plans/cache-server.md`](../plans/cache-server.md).

## Consequences

- One server, one store, one budget and one set of credentials serves every
  language an organisation builds. Artifacts dedupe across protocols because
  the core stores every body once, by content.
- The GitHub Action can run the server inside the job on `localhost`, with
  its store carried in `actions/cache` as today. Turbo, Nx, Gradle and
  ccache in the same job then share one cache without any external service.
- tinymold now owns a network service, with its own security surface:
  authentication, per-namespace write scopes, request limits and TLS. Those
  are part of the core, not left to each front-end.
- Cache quality for other languages is exactly as good as their own clients.
  tinymold adds speed, dedup and one place to run it, not better keys.
- An async runtime and an HTTP stack enter the server crate. `tinymold-hash`,
  `tinymold-cas` and the new store crate stay free of them.
- The crate graph gains `tinymold-store` (the engine) and `tinymold-server`
  (transport and front-ends). The existing crates keep their names, because
  crates.io names are permanent.
- Claims of "incredibly fast" need numbers: the server is benchmarked
  against bazel-remote on the same machine before it replaces it in the
  docs and the CI job.
