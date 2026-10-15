# tinymold in GitHub Actions

- **Status:** Implemented
- **Owner:** Steven Enamakel
- **Date:** 2026-10-14
- **Plan:** [`../plans/remote-and-ci.md`](../plans/remote-and-ci.md)
- **Builds on:** [`gc.md`](gc.md), [`remote.md`](remote.md),
  [ADR 0006](../adr/0006-remote-tier-reapi-bazel-remote.md)

## Problem

CI jobs build and test the same crates over and over. The common setup,
`Swatinem/rust-cache`, archives `target/` and the cargo registry once per
job. Before saving it deletes the workspace crates, and it leaves cargo's
mtime fingerprints to decide what to rebuild after a restore. As a result:

- **Workspace crates are always rebuilt.** That covers clippy and coverage
  runs too.
- **Changing the compiler or a build setting starts cold.** The archive's
  fallback key leaves out only the lockfile hash. A new rustc, `RUSTFLAGS`
  or any other `CARGO*`/`RUST*` variable needs a different archive.
- **Jobs don't share by default.** Each job gets its own archive unless it
  sets `shared-key`.

A lockfile change does not start cold there: the fallback key restores the
previous lockfile's archive, and cargo rebuilds the dependencies that
changed.

tinymold's unit cache keys every rustc call by content. A job that starts
with a populated store recompiles only the units whose inputs changed,
workspace crates included. Builds with different settings keep their
entries side by side in one store.

## Goals and non-goals

Goals:

- Two steps a workflow adds, around its existing cargo commands:
  - `uses: tinyhumansai/tinymold@v0` sets tinymold up and restores the
    cache;
  - `uses: tinyhumansai/tinymold/save@v0` with `if: always()` saves it.
  - `v0` is a moving tag that the release workflow points at the newest
    0.x release; pin a full tag such as `v0.1.0` to stay put.
- A stateless default that needs no server: the store travels in GitHub's
  own cache.
- An optional shared remote server ([`remote.md`](remote.md)), for many
  repositories or runners.
- Works for `cargo build`, `cargo test`, `cargo clippy` and
  `cargo llvm-cov`, on Linux and macOS runners, hosted or self-hosted.

Non-goals:

- **Windows runners.** The binary builds there, but the action is untested
  on them.
- **Caching `target/` itself.** Units come back as cache hits instead.
- **Making linking incremental.** A binary that depends on an edited crate
  is still relinked in full. Unchanged binaries and tests are restored
  without linking.

## Usage

```yaml
steps:
  - uses: actions/checkout@v7
  - uses: dtolnay/rust-toolchain@stable
  - uses: tinyhumansai/tinymold@v0
  - run: cargo test --all-features
  - run: cargo llvm-cov --workspace --lcov --output-path lcov.info
  - uses: tinyhumansai/tinymold/save@v0
    if: always()
```

With a remote server:

```yaml
  - uses: tinyhumansai/tinymold@v0
    with:
      backend: both
      remote-url: grpcs://cache.example.com
      remote-auth: ${{ secrets.TINYMOLD_REMOTE_AUTH }}
```

## Inputs

**Setup** (`action.yml`):

| Input | Default | Meaning |
|---|---|---|
| `version` | `latest` | Release tag to install, such as `v0.1.0` |
| `install` | `release` | `release` downloads the binary and checks it against `SHA256SUMS`; `source` runs `cargo install --locked` from `source-path`; `none` expects `tinymold` on `PATH` |
| `source-path` | `.` | With `install: source`, the path of a tinymold checkout |
| `backend` | `github` | `github`, `remote`, `both`, or `none` (local store only, for self-hosted runners with a persistent `store`) |
| `store` | `~/.cache/tinymold` | Store directory (`TINYMOLD_STORE`) |
| `cache-key` | `''` | Extra text mixed into the GitHub cache key, to separate jobs that build different things |
| `cache-registry` | `true` | With `github` and `both`, also keep cargo's downloads (`registry/index`, `registry/cache`, `git/db` under `CARGO_HOME`) in a GitHub cache entry of their own |
| `remote-url`, `remote-auth`, `remote-instance` | `''` | See [`remote.md`](remote.md) |
| `remote-signing-key`, `remote-trusted-keys` | `''` | Signed entries; see [`remote.md`](remote.md#signed-entries). Give the signing key only to trusted writers |
| `serve` | `false` | `true` runs `tinymold serve` for the rest of the job; see [Serving other tools](#serving-other-tools) |
| `serve-dir` | `~/.cache/tinymold-server` | The server's store directory (`TINYMOLD_SERVER_DIR`) |
| `serve-port` | `9090` | The loopback port it listens on |
| `serve-max-size` | `4G` | The server's `--max-size`; its shutdown trims to it |
| `turbo-team` | the repository owner | Exported as `TURBO_TEAM` |

The setup step also runs `tinymold doctor`, which shows the store and the
remote without the credential.

**Save** (`save/action.yml`):

| Input | Default | Meaning |
|---|---|---|
| `max-age` | `48h` | `tinymold gc --max-age` before saving |
| `max-size` | `4G` | `tinymold gc --max-size` before saving |
| `push` | `auto` | `auto` pushes when `remote-auth` was given; `true` and `false` force the choice |
| `save-cache` | `true` | Whether the `github` and `both` backends save the store and the registry |

## Behavior

**Setup:**

1. Install the binary and add it to `PATH`.
2. Export:
   - `RUSTC_WRAPPER=tinymold`;
   - `TINYMOLD_STORE`;
   - `TINYMOLD_LOG=$RUNNER_TEMP/tinymold.log`;
   - `CARGO_INCREMENTAL=0`;
   - `TINYMOLD_SAVE_START` (the job start, in Unix seconds);
   - with a remote, the `TINYMOLD_REMOTE_*` variables.

   The credential is masked in the log.
3. For `github` and `both`, restore the store with `actions/cache/restore`:
   - key:
     `tinymold-v1-<os>-<arch>-<cache-key>-<hash of rustc -vV>-<hash of Cargo.lock files>-<run id>-<attempt>`;
   - restore keys, from most to least specific: the same key without the
     run, then without the lockfile hash, then without the rustc hash.

   A job restores the newest store of the closest lockfile.
4. For `github` and `both` with `cache-registry`, restore cargo's downloads
   the same way, from their own entry:
   - key: `tinymold-cargo-v1-<os>-<arch>-<hash of Cargo.lock files>`;
   - restore key: the same without the lockfile hash.

   `registry/src` and `git/checkouts` are left out: cargo unpacks them
   again faster than the cache would download them.

**Save:**

1. When pushing, run
   `tinymold remote push --since $TINYMOLD_SAVE_START`.
2. Summarize the wrapper log into `$GITHUB_STEP_SUMMARY` with
   `tinymold stats --markdown`: hits, remote hits, misses, pass-throughs,
   errors, and wrapper time.
3. Run `tinymold gc` with `max-age` and `max-size`, for every backend: a
   self-hosted runner keeps its store between jobs whichever backend it
   uses.
4. For `github` and `both`, save the store with `actions/cache/save`, but
   only when the job stored something new. A job that only hit would upload
   the same store it restored.

   The key is the setup step's key, then the job id, then the first 12 hex
   digits of a hash of the store files written since setup. Every job of a
   run shares the setup key, and GitHub keeps only the first entry saved
   under a key, so without the suffix only the first job's new units would
   survive. Matrix legs share a job id but store different units, so the
   hash keeps them apart too. Two jobs that store the same units get the
   same key, and losing one of the two loses nothing.
5. With `cache-registry`, save cargo's downloads when no entry for this
   exact lockfile was restored. First, delete every crate archive in
   `registry/cache` that no `Cargo.lock` in the workspace names, so the
   entry does not grow with each lockfile it was restored across. With no
   lockfile, nothing is deleted. Jobs that race to save it save the same
   crates, so losing that race costs nothing. The first job to save a
   lockfile's entry decides what it holds; a job that builds only part of
   the workspace may save only part of the crates.

## Serving other tools

With `serve: true`, the job also gets a cache server for tools that speak
a remote-cache protocol ([`cache-server.md`](cache-server.md)), with no
service to run:

```yaml
steps:
  - uses: actions/checkout@v7
  - uses: tinyhumansai/tinymold@v0
    with:
      serve: true
  - run: npm ci && npx turbo run build test
  - uses: tinyhumansai/tinymold/save@v0
    if: always()
```

- **Setup** restores the server's store together with the wrapper store,
  as one GitHub cache entry naming both paths (`TINYMOLD_CACHE_PATHS`).
  Then it starts `tinymold serve` on `127.0.0.1:<serve-port>`, waits for
  `/healthz`, and exports, unless the workflow already set them:
  - `TURBO_API`, `TURBO_TOKEN` (a placeholder Turbo requires), `TURBO_TEAM`;
  - `NX_SELF_HOSTED_REMOTE_CACHE_SERVER`;
  - `CCACHE_REMOTE_STORAGE=http://127.0.0.1:<port>|layout=bazel`;
  - `TINYMOLD_SERVE_URL`, for Gradle (`<url>/gradle/`) and Bazel
    (`--remote_cache=grpc://127.0.0.1:<port>`).
- **The server is open, on loopback only.** It needs no token file: only
  processes on this runner can reach it.
- **Save** stops the server first. Its shutdown collects garbage to
  `serve-max-size` with no grace period, so the saved entry is already
  trimmed. A job counts as having stored something new, and saves, when
  either the wrapper or the server wrote a file; the entry's key hashes
  both stores' new files.
- **A Rust toolchain is optional.** Without `rustc` the cache key uses
  `no-rustc` in place of the compiler hash.
- **Not yet:** reading through to a remote server. With `serve: true` and a
  remote, the server serves this job's store only; the wrapper's remote
  tier is unchanged.

## Why one archive per job, not one GitHub cache entry per object

sccache's GitHub Actions mode stores each object as its own cache entry,
through the cache service API that `actions/cache` uses internally.
tinymold does not:

- **The per-entry API is internal.** It is not a documented interface, and
  it has already changed once (cache service v2).
- **It is rate-limited.** A clean build of a medium workspace writes
  thousands of objects.

One archive per job costs one download and one upload. `tinymold gc` keeps
it under the repository's limit, and its mtimes carry last-use times from
one run to the next.

GitHub evicts whole cache entries least recently used first, once a
repository exceeds 10 GB. With `max-size: 4G`, about two generations of
the store fit beside each other. Each job that stores something saves an
entry of its own. A workflow with many such jobs should give them a
smaller `max-size`, or save only from the default branch with
`save-cache: ${{ github.ref == 'refs/heads/main' }}`. Pull requests can
still restore entries from the default branch.

## Coverage, clippy, and incremental

- **`cargo llvm-cov`.** It instruments only workspace crates, through its
  own wrapper that chains to tinymold, so dependencies share entries with
  plain test builds. Instrumented workspace crates carry
  `-C instrument-coverage` and never share entries with plain ones. Coverage mappings embed the
  absolute path of the workspace, so the pre-key of an instrumented compile
  includes the real current directory. Coverage from a cache hit then
  always points at the right files. On hosted runners the checkout path
  never changes, so this costs no hits.
- **`cargo clippy`.** Clippy wraps workspace crates with `clippy-driver`.
  Those calls are cached too, in entries of their own that replay the lints,
  so a re-run lints only the crates whose inputs changed.
- **Incremental builds.** `CARGO_INCREMENTAL=0` is the CI norm, and it keeps
  a job from adding incremental artifacts that later jobs would never
  reuse.

## Self-hosted runners

- Set `backend: none` and point `store` at a persistent disk shared by the
  runners on that machine. The store is safe for concurrent use.
- Keep the save step: it runs `tinymold gc`, so the store stays bounded
  without a cron job.
- Add a remote server with `backend: remote` to share the cache between
  machines.

## Acceptance criteria

- **This repository's CI runs the action on itself** (jobs `action-fill`
  and `action-reuse`):
  1. `action-fill` builds and tests, and saves the store;
  2. `action-reuse` restores it on a fresh runner and builds the same
     commit;
  3. that second build reaches ≥ 90% hits, checked with
     `tinymold stats --min-hit-rate 90`.
- The `action-scripts` job runs `action/test.sh` on Linux and macOS.
  The test checks that jobs storing different units name different
  entries, and which crate archives the registry keeps.
- The `remote-e2e` job does the same through a bazel-remote
  container, with the local store wiped in between.
