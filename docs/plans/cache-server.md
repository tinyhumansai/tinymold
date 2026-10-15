# Plan: the cache server and its front-ends

- **Spec:** [`../specs/cache-server.md`](../specs/cache-server.md)
- **Decision:** [ADR 0009](../adr/0009-a-cache-server-with-protocol-front-ends.md)
- **Status:** In progress (slices 1-7 done)

## Goal

Ship `tinymold serve`: one store, several protocols, faster than
bazel-remote, and usable from a GitHub Actions job with no external service.

## Non-goals

Everything listed as a non-goal in the spec, plus: no new Rust adapter
features in this plan. The rustc wrapper changes only in slice 9, to use the
native socket.

## Assumptions

- The local store layout (`tinymold-cas`) does not change. The server's
  store is a `tinymold-cas` store with an index beside it, so one machine
  can share it between `tinymold serve` and the wrapper.
- The existing `gc` module in `crates/tinymold` moves into `tinymold-store`
  rather than being rewritten, so its tests come with it.
- The HTTP stack is one dependency chosen in slice 2 (hyper 1 with
  hyper-util is the expected choice, since tonic already pulls it in). The
  choice is recorded with its build-time cost in the PR.

## Crate layout after this plan

```text
crates/
├── tinymold-hash/      # unchanged
├── tinymold-cas/       # unchanged
├── tinymold-store/     # new: namespaces, entries, digest index, last use, GC
├── tinymold-server/    # new: transport, auth, front-ends
│   └── src/front/{reapi,bazel_http,turbo,nx,gradle,native}/
├── tinymold-key/       # the Rust adapter: keys (unchanged)
├── tinymold-wrap/      # the Rust adapter: rustc wrapper (slice 9 only)
├── tinymold-remote/    # REAPI client (unchanged)
└── tinymold/           # CLI: adds `serve`, `serve status`, `gocacheprog`
```

## Slices

Each slice is one PR, test first, with the four contract commands green.

### 1. `tinymold-store`: namespaces and entries

- Files: `crates/tinymold-store/src/{lib.rs,error/mod.rs,namespace/,entry/,index/}`.
- Failing tests first:
  - the store key of a fixed `(front-end, tenant, key)` matches a pinned
    BLAKE3 vector;
  - an entry record round-trips and its encoding matches a pinned byte
    vector;
  - the same key bytes in two namespaces are two entries;
  - an entry naming a missing object is refused (`Error::MissingObject`);
  - ingest of a body yields both its BLAKE3 name and its SHA-256, and the
    SHA-256 index finds it;
  - a body with a claimed digest that does not match is refused
    (`Error::DigestMismatch`) and leaves nothing in the store.
- Then: the implementation over `tinymold-cas`. **As built:** one file per
  entry under `ac/` and one per SHA-256 under `sha256/`, each written
  privately and renamed into place, instead of an append log. A reader sees
  a whole file or none, several processes share one root without a lock,
  and a restart needs no replay. An in-memory index can sit in front of it
  later if the slice 11 benchmark asks for one. Streaming ingest needed a
  new `tinymold_cas::Store::stage`, which hashes a body while staging it and
  commits it only when asked.

### 2. `tinymold-store`: last use, pins and GC

- ~~Move `crates/tinymold/src/gc` into `tinymold-store`.~~ **As built:** that
  collector walks the wrapper's records and snapshots, so moving it would
  make the store depend on `tinymold-wrap`. The store has its own collector,
  `tinymold_store::collect`, over its own layout, with the same model: mtime
  is last use, an age limit, an LRU byte budget and a grace period. A store
  root and a wrapper store root are never the same directory.
- Failing tests first: a pinned object survives GC; an object shared by two
  entries survives evicting one; a stale file in `pending/` is swept.

### 3. `tinymold-server`: transport, auth, health

- Files: `crates/tinymold-server/src/{lib.rs,error/,listen/,auth/,route/,metrics/}`
  and `tinymold serve` in `crates/tinymold/src/serve/`.
- Failing tests first, against a server on an ephemeral port in a tempdir:
  `/healthz` answers 200; a missing or wrong token is 401; a read-only token
  writing is 403; a token never appears in a log line or in `/metrics`.
- Record the HTTP dependency choice and its clean-build cost in the PR.

**As built, slices 3 and 4 landed together**: without a front-end, the
access checks and request counters of slice 3 had no caller. The HTTP stack
is hyper 1 with hyper-util's auto builder (HTTP/1.1, h2 and h2c on one
listener) and tokio-rustls, all crates tonic already compiled for the
REAPI client. Bodies stream through a bounded channel between hyper and
the store's blocking reads and writes (`src/transfer/`). `tinymold serve`
takes `--dir`, not `--store`: the global `--store` names the wrapper's
store, which the server refuses to share.

### 4. `bazel-http` front-end

- `GET`/`PUT`/`HEAD` on `/ac/` and `/cas/`.
- Failing tests first: put then get round-trips; `/cas/` with a wrong
  digest is 400 and not stored; a body over the limit is refused before it
  is read.
- `live_ccache` test: a C file built twice with ccache's HTTP storage in
  the bazel layout hits on the second build.

### 5. `reapi` front-end

- Capabilities, ActionCache, CAS (`FindMissingBlobs`, batch read and
  update), ByteStream. Reuse the message types in `tinymold-remote` by
  moving them to a shared module rather than duplicating them.
- Failing tests first: `tinymold-remote`'s client against the server
  reproduces every `remote.md` acceptance criterion; a blob uploaded through
  `bazel-http` is found by `FindMissingBlobs`.
- Switch the CI `remote-e2e` job to `tinymold serve`, keeping a second job
  against bazel-remote so REAPI compatibility stays tested.
- **As built:** the server declares its own messages
  (`src/front/reapi/types.rs`) instead of sharing `tinymold-remote`'s,
  because that crate depends on `tinymold-wrap` and because the server
  keeps action results as raw bytes, so fields it does not declare survive.
  Both sets are pinned against encoded bytes. CI gained a
  `remote-e2e-serve` job beside the bazel-remote one; run locally against
  this workspace's `tinymold-hash` and `tinymold-cas`, the rebuild from an
  empty store served 22 of 24 cacheable compiles (91.7%) from the server.

### 6. `turbo` front-end

- `/v8/artifacts` per Turborepo's OpenAPI spec, with team as tenant and the
  artifact headers in `meta`.
- Failing tests first: the status endpoint, put/get/head, the batch query,
  team isolation, and headers returned unchanged.
- `live_turbo` test: a two-package fixture under `crates/tinymold-server/tests/fixtures/`
  gets `FULL TURBO` on the second run.
- **As built:** passes against turbo 2.11.8 from npm. The fixture copy is
  made a git repository that ignores build output: outside git, turbo
  hashes every file in a package, its own `.turbo` logs included, and no
  two runs share a task hash. The batch query's JSON is parsed and written
  by hand; it is a list of hex strings in and a flat map out, which does
  not justify a JSON dependency.

### 7. `nx` and `gradle` front-ends

- Nx: `/v1/cache/{hash}` per the Nx OpenAPI spec, including its rule for a
  `PUT` over an existing entry, pinned by a test.
- Gradle: `/gradle/{key}`, `Expect: 100-continue`, `413` over the limit.
- `live_nx` and `live_gradle` tests on fixtures, as in slice 6.
- **As built:** Nx records are immutable, as its spec requires: a `PUT` to
  a stored hash is `409` before the body is read. The three
  single-artifact protocols (Turbo, Nx, Gradle) share
  `src/front/artifact/`, and the live tests share `tests/common/`. Both
  live tests pass: Nx 21 restores `[remote cache]` with a write token, and
  Gradle 9.8.1 loads `:compileJava` from the server with basic auth.

### 8. GitHub Action `serve: true`

- `action.yml` and `save/`: start the server, export each front-end's
  client variables, stop it and GC on save.
- A workflow in `.github/workflows/` runs the Turbo fixture under the
  action twice and asserts the second run is fully cached.
- Update [`../specs/github-actions.md`](../specs/github-actions.md).

### 9. `native` socket and the Rust adapter

- The local Unix-socket protocol: keys, paths and digests, with the server
  materializing into `target/`. Single-flight per key across clients.
- `tinymold-wrap` uses the socket when it is present, and today's
  in-process path when it is not. The existing wrapper tests run in both
  modes.
- Failing test first: two wrappers asking for the same missing key run
  rustc once.

### 10. `gocacheprog` adapter

- `tinymold gocacheprog`: the JSON-lines protocol on stdin and stdout,
  `get` answering with a materialized local path.
- Failing tests first: protocol fixtures for `get` hit, `get` miss, `put`
  and `close`. `live_go` test: `go build` twice hits on the second.

### 11. Benchmark and the switch

- `bench/bench.py server`: hit latency, throughput, `FindMissingBlobs`,
  ingest and cold start, against bazel-remote on the same machine. Python
  standard library only.
- Commit results for darwin arm64 and Linux, regenerate `bench/README.md`
  with `bench/report.py`.
- If every spec target is met, switch `deploy/` to `tinymold serve` and keep
  the bazel-remote files as an alternative.

## Verification

Every slice:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --all-targets --all-features
cargo test --all-features
```

Live tests, with the toolchains installed:

```sh
TINYMOLD_LIVE=1 cargo test -p tinymold-server live_
```

## Checklist

- [x] 1. Store: namespaces and entries
- [x] 2. Store: last use, pins and GC
- [x] 3. Server: transport, auth, health
- [x] 4. `bazel-http` front-end
- [x] 5. `reapi` front-end and the CI switch
- [x] 6. `turbo` front-end
- [x] 7. `nx` and `gradle` front-ends
- [ ] 8. GitHub Action `serve: true`
- [ ] 9. Native socket and the Rust adapter
- [ ] 10. `gocacheprog` adapter
- [ ] 11. Benchmark and the switch
