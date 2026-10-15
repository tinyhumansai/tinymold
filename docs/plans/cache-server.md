# Plan: the cache server and its front-ends

- **Spec:** [`../specs/cache-server.md`](../specs/cache-server.md)
- **Decision:** [ADR 0009](../adr/0009-a-cache-server-with-protocol-front-ends.md)
- **Status:** In progress (slice 1 done)

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

- Move `crates/tinymold/src/gc` into `tinymold-store`, keeping its tests;
  `tinymold gc` calls it from there.
- Failing tests first: a pinned object survives GC; an object shared by two
  entries survives evicting one; a stale file in `pending/` is swept.

### 3. `tinymold-server`: transport, auth, health

- Files: `crates/tinymold-server/src/{lib.rs,error/,listen/,auth/,route/,metrics/}`
  and `tinymold serve` in `crates/tinymold/src/serve/`.
- Failing tests first, against a server on an ephemeral port in a tempdir:
  `/healthz` answers 200; a missing or wrong token is 401; a read-only token
  writing is 403; a token never appears in a log line or in `/metrics`.
- Record the HTTP dependency choice and its clean-build cost in the PR.

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

### 6. `turbo` front-end

- `/v8/artifacts` per Turborepo's OpenAPI spec, with team as tenant and the
  artifact headers in `meta`.
- Failing tests first: the status endpoint, put/get/head, the batch query,
  team isolation, and headers returned unchanged.
- `live_turbo` test: a two-package fixture under `crates/tinymold-server/tests/fixtures/`
  gets `FULL TURBO` on the second run.

### 7. `nx` and `gradle` front-ends

- Nx: `/v1/cache/{hash}` per the Nx OpenAPI spec, including its rule for a
  `PUT` over an existing entry, pinned by a test.
- Gradle: `/gradle/{key}`, `Expect: 100-continue`, `413` over the limit.
- `live_nx` and `live_gradle` tests on fixtures, as in slice 6.

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
- [ ] 2. Store: last use, pins and GC
- [ ] 3. Server: transport, auth, health
- [ ] 4. `bazel-http` front-end
- [ ] 5. `reapi` front-end and the CI switch
- [ ] 6. `turbo` front-end
- [ ] 7. `nx` and `gradle` front-ends
- [ ] 8. GitHub Action `serve: true`
- [ ] 9. Native socket and the Rust adapter
- [ ] 10. `gocacheprog` adapter
- [ ] 11. Benchmark and the switch
