# Plan: last-use GC, the remote tier, and the GitHub Action

- **Specs:** [`../specs/gc.md`](../specs/gc.md),
  [`../specs/remote.md`](../specs/remote.md),
  [`../specs/github-actions.md`](../specs/github-actions.md)
- **Decision:** [ADR 0006](../adr/0006-remote-tier-reapi-bazel-remote.md)
- **Status:** Implemented

## Order of implementation

Each step lands with its tests first, and leaves the four contract commands
green.

1. **Last use** (`crates/tinymold-wrap/src/compile/`). Every hit sets the
   mtime of its action and pre-key records. Test: a hit moves the action's
   mtime forward.
2. **Store listing** (`crates/tinymold-cas/src/store/`).
   `Store::objects()` yields each object's digest, size and mtime;
   `Store::remove` deletes one object; `Store::staging_files()` lists
   `tmp/`. They hold no policy.
3. **Record catalog** (`crates/tinymold-wrap/src/catalog/`). A public listing
   of actions and pre-key records with their last use, and the digests an
   action references. GC and push both use it.
4. **`tinymold gc`** (`crates/tinymold/src/gc/`):
   - duration and size parsers;
   - expiry by age;
   - LRU size budget with reference counts;
   - the cache-file and object sweeps with a grace period;
   - snapshot expiry;
   - `--dry-run`.

   Then the store size in `doctor`. Tests use `tempfile` stores with
   back-dated mtimes.
5. **`tinymold stats`** (`crates/tinymold/src/stats/`): parses the wrapper
   log and renders text or Markdown.
6. **The coverage key rule** (`crates/tinymold-key/src/key/`). An
   instrumented compile's pre-key includes the absolute current directory.
   Test: two roots with coverage give different keys, and without coverage
   they give the same key.
7. **`crates/tinymold-remote`:**
   - hand-written prost messages, with field numbers pinned against encoded
     bytes;
   - the tonic client (capabilities, AC get and update, find-missing, and
     ByteStream read and write);
   - the SHA-256 translation;
   - an implementation of the wrapper's `Remote` trait, and `push`.

   Tests run an in-process REAPI server on a random local port.
8. **The wrapper's remote lookup**:
   - `tinymold_wrap::run_with_remote`;
   - the `remote` log outcome;
   - `TINYMOLD_REMOTE_*` read by `tinymold`;
   - `tinymold remote push|check`.
9. **The actions**: `action.yml` and `save/action.yml`, the CI jobs
   `action-fill`, `action-reuse` and `remote-e2e`, and
   `deploy/bazel-remote/`.
10. **README and bench**: a "Use in GitHub Actions" section, and a `ci`
    scenario in `bench/bench.py` with committed results.

## Completion checklist

- [x] Steps 1-6 (local GC, stats, coverage rule)
- [x] Steps 7-8 (remote tier)
- [x] Step 9 (actions, CI, deployment)
- [x] README section
- [x] Step 10 bench numbers

## Measured (`bench/bench.py ci`, darwin arm64, 18 CPUs, 3 runs)

| Mode | ripgrep | bat | tinymold |
|---|---:|---:|---:|
| Cold | 3.9s | 9.9s | 7.6s |
| `target/` restored, sources touched | 1.8s | 3.3s | 3.3s |
| tinymold store restored | 1.9s | 6.7s | 3.8s |
| tinymold from bazel-remote | 1.4s | 7.1s | 2.9s |

tinymold beats a cold build by 2-3x everywhere. It does not yet beat a
`target/` cache: cargo re-runs every build script in an empty `target/`, and
bat's compile C code, which `RUSTC_WRAPPER` never sees. Caching build-script
runs, or restoring `target/` alongside the store (`tinymold fresh` already
makes that safe), is the next step.

## Found by running against bazel-remote

- **AC keys need a size.** bazel-remote refuses `size_bytes = 0` for any
  hash but the empty blob's ("Invalid zero-length SHA256 hash"), so keys are
  sent with size 32. The fake test server now enforces the same rule.
- **`GetCapabilities` does not test a credential.** With
  `--allow_unauthenticated_reads` it answers anyone, so `remote check` also
  uploads a probe blob.
- **Paths still matter across target directories.** A second build into
  `target/e2e-b` got 83.5% remote hits, against 91.4% into the same
  `target/e2e-a`. Native objects from build scripts (blake3's C code) embed
  the out-dir path, so their dependents' keys differ. That is a correct miss.
  CI runners build at the same path every run.
