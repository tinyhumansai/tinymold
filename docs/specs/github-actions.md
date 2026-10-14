# tinymold in GitHub Actions

- **Status:** Implemented
- **Owner:** Steven Enamakel
- **Date:** 2026-10-14
- **Plan:** [`../plans/remote-and-ci.md`](../plans/remote-and-ci.md)
- **Builds on:** [`gc.md`](gc.md), [`remote.md`](remote.md),
  [ADR 0006](../adr/0006-remote-tier-reapi-bazel-remote.md)

## Problem

CI jobs build and test the same crates over and over. The common setup
(`Swatinem/rust-cache`) restores `target/` per lockfile, so:

- any change to `Cargo.lock` starts cold;
- the restore ignores which units actually changed;
- a checkout's fresh mtimes make cargo recompile workspace crates anyway.

tinymold's unit cache keys every rustc call by content, so a job that
starts with a populated store recompiles only the units whose inputs
changed.

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
| `remote-url`, `remote-auth`, `remote-instance` | `''` | See [`remote.md`](remote.md) |

The setup step also runs `tinymold doctor`, which shows the store and the
remote without the credential.

**Save** (`save/action.yml`):

| Input | Default | Meaning |
|---|---|---|
| `max-age` | `7d` | `tinymold gc --max-age` before saving |
| `max-size` | `4G` | `tinymold gc --max-size` before saving |
| `push` | `auto` | `auto` pushes when `remote-auth` was given; `true` and `false` force the choice |
| `save-cache` | `true` | Whether the `github` and `both` backends save the store |

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

**Save:**

1. When pushing, run
   `tinymold remote push --since $TINYMOLD_SAVE_START`.
2. Summarize the wrapper log into `$GITHUB_STEP_SUMMARY` with
   `tinymold stats --markdown`: hits, remote hits, misses, pass-throughs,
   errors, and wrapper time.
3. Run `tinymold gc` with `max-age` and `max-size`, for every backend: a
   self-hosted runner keeps its store between jobs whichever backend it
   uses.
4. For `github` and `both`, save the store with `actions/cache/save` under
   the setup step's exact key, but only when the job stored something new.
   A job that only hit would upload the same store it restored.

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
the store fit beside each other.

## Coverage, clippy, and incremental

- **`cargo llvm-cov`.** Instrumented builds carry `-C instrument-coverage`,
  so they never share entries with plain ones. Coverage mappings embed the
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
- The `remote-e2e` job does the same through a bazel-remote
  container, with the local store wiped in between.
