# The cache server

- **Status:** Accepted
- **Owner:** Steven Enamakel
- **Date:** 2026-10-15
- **Plan:** [`../plans/cache-server.md`](../plans/cache-server.md)
- **Builds on:** [ADR 0009](../adr/0009-a-cache-server-with-protocol-front-ends.md),
  [ADR 0002](../adr/0002-blake3-for-content-digests.md),
  [ADR 0005](../adr/0005-trade-disk-for-time.md), [`gc.md`](gc.md),
  [`remote.md`](remote.md)

## Problem

Every build ecosystem has a remote cache protocol and, usually, its own
server for it. An organisation that builds Rust, TypeScript, Java and C++
runs four caches. Each has its own disk, eviction and credentials, and
identical bytes are stored once per server. The Rust side also needs a hot,
machine-local server (single-flight across worktrees, a stat cache) that no
existing remote provides.

## Goals and non-goals

Goals:

1. One server process, `tinymold serve`, that serves several cache protocols
   from one store, on one port, with one budget.
2. Fast enough to be worth running locally as well as remotely. Targets are
   under [Performance](#performance).
3. Correct storage: a reader gets exactly the bytes stored under a key, or a
   miss. Never a partial body, never another namespace's entry.
4. Runs the same way on a developer machine, in a GitHub Actions job, and as
   a shared service on a VM or Kubernetes.

Non-goals:

- **Computing keys for other languages.** Each toolchain's client does
  that. The server cannot tell whether a hit is correct, only whether it was
  stored.
- **Remote execution.** No REAPI `Execution` service.
- **A build system or task runner.** tinymold does not schedule work.
- **Replacing the clients.** Turbo, Nx, Gradle, ccache, Bazel and Buck2 are
  used unchanged, configured through their documented remote-cache settings.

## Architecture

```text
 Turbo   Nx   Gradle   ccache   Bazel/Buck2   tinymold (Rust)   go (GOCACHEPROG)
   │      │      │        │          │               │                 │
   │ HTTP │ HTTP │  HTTP  │ HTTP     │ gRPC          │ gRPC / socket   │ via `tinymold gocacheprog`
   ▼      ▼      ▼        ▼          ▼               ▼                 ▼
 ┌───────────────────────── tinymold serve ───────────────────────────────┐
 │ transport: one listener (HTTP/1.1, h2, h2c, TLS) + local Unix socket   │
 │ auth: tokens → (namespace, read | write)                               │
 │ front-ends: turbo │ nx │ gradle │ bazel-http │ reapi │ native          │
 ├────────────────────────────────────────────────────────────────────────┤
 │ tinymold-store: namespaces, ActionCache, digest index, last use, GC   │
 ├────────────────────────────────────────────────────────────────────────┤
 │ tinymold-cas: objects by BLAKE3, atomic insert, reflink/hardlink/copy  │
 └────────────────────────────────────────────────────────────────────────┘
```

Three layers, dependencies pointing down:

- **`tinymold-cas`** (exists): immutable objects named by BLAKE3.
- **`tinymold-store`** (new): everything above raw objects that every
  protocol shares. No network, no async runtime.
- **`tinymold-server`** (new): the transport, authentication and the
  front-ends. The only crate besides `tinymold-remote` with an async runtime.

## The core data model

### Namespaces

Every key lives in a **namespace**: a front-end identifier plus a tenant
string.

```text
namespace = (front-end, tenant)
  front-end: "reapi" | "bazel-http" | "turbo" | "nx" | "gradle" | "native"
  tenant:    REAPI instance name, Turbo team, Nx/Gradle/bazel-http URL prefix
```

Two protocols that send the same key bytes never share an entry. Tenants
separate repositories or teams that should not read each other's results.

### ActionCache entries

The store keys each entry by:

```text
store key = BLAKE3(field "tinymold/store/ac/v1", front-end, tenant, client key bytes)
```

so client keys of any length (Turbo's 16 hex digits, Gradle's 32, REAPI's
SHA-256) become one fixed-size index key. The value is an **entry record**:

| Field | Meaning |
|---|---|
| `blobs` | Ordered list of object digests (BLAKE3) the entry needs. One for single-artifact protocols; one per output for REAPI |
| `body` | For REAPI, the serialized `ActionResult` bytes as the client sent them. Empty for single-artifact protocols |
| `meta` | Small string map the front-end must hand back on a read (Turbo's `x-artifact-duration` and `x-artifact-tag`). At most 4 KiB |
| `written` | Unix seconds at write, for status and debugging only. Never part of a key |

The record encoding is versioned and pinned by a golden-vector test. An
entry is written only after every object in `blobs` is committed, so a
reader that finds an entry can always serve it.

### The digest index

Clients name blobs with different hash functions. REAPI, Bazel HTTP and
bazel-remote's world use SHA-256; tinymold uses BLAKE3. The store keeps an
index from `(function, digest)` to the BLAKE3 object name:

- **Ingest computes both digests in one pass** over the body, BLAKE3 for the
  object name and SHA-256 for the index, so a REAPI `FindMissingBlobs` for a
  blob that arrived through Turbo still finds it.
- A body that claims a digest (REAPI CAS, Bazel `/cas/`) is rejected unless
  the computed digest matches. A body under an opaque key (Turbo, Nx,
  Gradle, Bazel `/ac/`) has no claim to check.

ADR 0002's rule still holds: tinymold's own structures name objects by
BLAKE3, and SHA-256 appears only in the index and on the wire.

### Last use and eviction

Every read and write records last use per entry and per object, as
[`gc.md`](gc.md) does for the local store today. Eviction is LRU by entry
within a global byte budget (`--max-size`), with an optional age limit.
An object is removed only when no surviving entry references it. Eviction
never removes an object that a read is streaming or a write is about to
reference: writers pin the objects they upload until their entry is
committed or the upload is abandoned.

Objects are stored uncompressed on disk ([ADR 0005](../adr/0005-trade-disk-for-time.md)).
Compression happens on the wire only where a protocol negotiates it.

## Protocol front-ends

A front-end is a module of `tinymold-server` behind its own cargo feature.
It parses one protocol's requests, maps them to namespace, key and entry
operations, and formats the protocol's responses. Front-ends hold no state
of their own, and none of them reaches the disk except through
`tinymold-store`.

| Front-end | Feature | Wire | Clients | Mapping |
|---|---|---|---|---|
| `reapi` | `reapi` | gRPC: Capabilities, ActionCache, ContentAddressableStorage, ByteStream | tinymold (Rust), Bazel, Buck2, Pants, BuildBuddy and other REAPI clients | AC → entry with one blob per output and the `ActionResult` as `body`; CAS → objects via the SHA-256 index |
| `bazel-http` | `bazel-http` | HTTP `GET`/`PUT`/`HEAD` on `/ac/<sha256>` and `/cas/<sha256>` | Bazel `--remote_cache=http…`, ccache HTTP storage with the bazel layout | `/cas/` → object, digest checked; `/ac/` → single-blob entry |
| `turbo` | `turbo` | HTTP on `/v8/artifacts`: `GET`, `HEAD`, `PUT /{hash}`, `POST` (batch query), `GET /status` | Turborepo (`TURBO_API`, `TURBO_TOKEN`, `TURBO_TEAM`) | `{hash}` → single-blob entry, tenant = team; artifact headers kept in `meta` |
| `nx` | `nx` | HTTP `GET`/`PUT` on `/v1/cache/{hash}`, bearer token | Nx 20.8+ (`NX_SELF_HOSTED_REMOTE_CACHE_SERVER`) | `{hash}` → single-blob entry; a `PUT` over an existing entry is refused, as Nx's spec expects |
| `gradle` | `gradle` | HTTP `GET`/`PUT` on `/gradle/{key}`, basic auth | Gradle's `HttpBuildCache` | `{key}` → single-blob entry; honours `Expect: 100-continue` and answers `413` over the size limit |
| `native` | always | Unix socket, local only | tinymold's Rust adapter | Keys, paths and digests only; the server materializes objects into the caller's `target/` by reflink or hardlink instead of sending bytes |

Rules every front-end follows:

- **Unknown is a miss.** A malformed key, an unknown tenant or a request the
  front-end does not support answers with the protocol's miss or error
  status. It never falls through to another namespace.
- **Streaming, not buffering.** Bodies stream from the socket into a store
  temp file while both digests are computed, and stream from the object file
  to the socket on a read. No front-end holds a whole artifact in memory.
- **Size limits per front-end**, configurable, checked before a body is
  accepted (`Content-Length`) and again while streaming.
- **The protocol's own semantics win.** Where a protocol defines a status
  code or overwrite rule, the front-end follows it, and the integration test
  for that front-end pins it.
- **Protocol references.** The mappings above follow the published
  specifications: Turborepo's
  [Remote Cache API](https://turborepo.dev/docs/openapi), Nx's
  [self-hosted caching](https://nx.dev/docs/guides/tasks--caching/self-hosted-caching)
  OpenAPI spec, Gradle's HTTP build cache, Bazel's
  [HTTP caching protocol](https://bazel.build/remote/caching), and REAPI v2.
  Each front-end's module `README.md` links the exact version it was
  written against.

### Client-side adapters

Some toolchains talk to a local program rather than a URL. Their adapter
ships in the `tinymold` binary and talks to the server:

- **Rust:** `RUSTC_WRAPPER=tinymold` ([`rustc-wrapper.md`](rustc-wrapper.md)).
  It uses the `native` socket when a local server is running, REAPI when a
  remote is configured, and works with no server at all.
- **Go:** `GOCACHEPROG="tinymold gocacheprog"`. Go runs it as a subprocess
  speaking JSON lines (`get`, `put`, `close`) on stdin and stdout. A `get`
  hit must answer with a path on local disk, so the adapter materializes the
  object into the local store and returns that path. Its namespace is
  `native` with a Go tenant, and Go's `ActionID` and `OutputID` are kept in
  the entry record.

### Not yet supported

- **sccache.** Its WebDAV backend needs `PROPFIND` and directory semantics
  that have not been checked against this design. Revisit after the HTTP
  front-ends land.
- **Gradle and Turbo artifact signing.** Signed artifacts are stored and
  returned unchanged with their tag in `meta`; the server does not verify
  signatures, the client does.

## Transport and authentication

- **One listener** (`--listen`, default `127.0.0.1:9090`) serves HTTP/1.1,
  HTTP/2 over TLS, and h2c. gRPC requests (by `content-type`) go to `reapi`;
  everything else is routed by path prefix to the HTTP front-ends.
- **TLS** with `--tls-cert` and `--tls-key`, or terminated at an ingress, as
  `deploy/` already does for bazel-remote.
- **Tokens.** A token file maps each token to scopes:
  `(front-end | *, tenant | *, read | write)`. Bearer tokens (Turbo, Nx) and
  basic auth (Gradle, REAPI, Bazel HTTP) are both accepted, the password of
  basic auth being the token. `--allow-anonymous-reads` keeps today's
  bazel-remote setup for forks.
- **The native socket** is created with mode `0600` in the user's runtime
  directory and is trusted as the user. It is never exposed on a network.
- Credentials never appear in logs, `status` output or error messages.

## Operations

```text
tinymold serve [--listen ADDR] [--store DIR] [--max-size SIZE] [--max-age AGE]
               [--tokens FILE] [--allow-anonymous-reads] [--tls-cert F --tls-key F]
               [--front-ends reapi,bazel-http,turbo,nx,gradle]
tinymold serve status [--url URL]    # entries, bytes, hit rate per namespace
```

- **Persistence.** The index and last-use times live in the store directory
  and survive a restart. A restart serves hits immediately; it does not
  rebuild the index by scanning objects.
- **Crash safety.** A crash loses at most uncommitted uploads. Every entry
  and index file is written privately and renamed into place, so a torn
  write leaves only a stale file in `pending/`, which GC sweeps.
- **Metrics.** `GET /metrics` in Prometheus text format: requests, hits,
  misses and bytes per front-end and namespace, latency histograms, store
  bytes and evictions.
- **Health.** `GET /healthz` answers `200` once the index is loaded.

### In GitHub Actions

The setup action gains `serve: true`. It starts `tinymold serve` on
`localhost` for the rest of the job, with its store restored from
`actions/cache` as the store is today, and exports each enabled front-end's
client settings, for example:

```text
TURBO_API=http://127.0.0.1:9090  TURBO_TOKEN=…  TURBO_TEAM=ci
NX_SELF_HOSTED_REMOTE_CACHE_SERVER=http://127.0.0.1:9090
CCACHE_REMOTE_STORAGE=http://127.0.0.1:9090|layout=bazel
```

The save action stops the server, runs GC to the budget, and saves the
store. With `remote-url` set, the local server reads through to the remote
on a miss and the save step pushes new entries, as `tinymold remote push`
does for Rust today.

## Invariants

- A read returns either the complete bytes of a committed entry in the
  requested namespace, or a miss. Never a partial body.
- An entry is committed only after every object it references is committed.
- A body that claims a digest is stored only if the digest matches.
- An object referenced by a surviving entry, or pinned by an in-flight
  upload, is never evicted.
- Nothing about a request's time, client address or credential enters a
  key or an entry record, except `written`, which no lookup reads.
- `tinymold-store` and below have no network or async dependencies.

## Performance

Measured with a new `server` benchmark in `bench/bench.py`, against
bazel-remote on the same machine and disk, with results committed. Targets:

| Measure | Target |
|---|---|
| Hit, 1 KiB entry, localhost, HTTP/1.1 keep-alive | p50 ≤ 0.2 ms, p99 ≤ 1 ms server time |
| Hit throughput, 4 MiB blobs from page cache | ≥ the loopback rate bazel-remote achieves on the same machine |
| `FindMissingBlobs`, 10 000 digests | ≤ 5 ms |
| Ingest, 4 MiB blob | ≤ 1.2x the time to write it to disk once |
| Cold start with 1 M entries | serving within 1 s |
| Peak memory | does not grow with blob size |

The server does not replace bazel-remote in `deploy/` or the CI
`remote-e2e` job until it meets these on Linux and macOS.

## Acceptance criteria

- The CI `remote-e2e` job passes against `tinymold serve` in place of
  bazel-remote, reaching the same ≥ 90% remote outcomes ([`remote.md`](remote.md)).
- A fixture Turborepo monorepo, a fixture Nx workspace, a fixture Gradle
  build and a ccache build each get full cache hits on a second run against
  the server (`live_*` tests, gated by an env var).
- A blob uploaded through Turbo is reported present by REAPI
  `FindMissingBlobs` under its SHA-256, and is stored once on disk.
- A Turbo key written under team A misses for team B, and a key written
  through `nx` misses through `turbo`.
- A `/cas/` or REAPI CAS body with a wrong digest is rejected and not
  stored.
- A read-only token cannot write through any front-end.
- Killing the server mid-upload leaves no entry for that upload and a store
  that `tinymold cas verify` accepts.
- The performance targets above are met and the results committed.

## Open questions

- **Shared server trust.** A per-machine server is trusted as the user. A
  shared one trusts whoever holds write tokens; is per-namespace write scope
  enough, or do tenants need separate stores?
- **Read-through tiers.** Should a local server read through to a remote
  for every front-end, or only for `native` and `reapi` at first?
- **Wire compression.** REAPI's compressed-blobs (zstd) and HTTP
  `Content-Encoding`: worth negotiating on localhost, where it only costs?
- **Windows.** The native socket needs a named-pipe equivalent.
