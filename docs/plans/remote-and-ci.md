# Plan: last-use GC, the remote tier, and the GitHub Action

- **Specs:** [`../specs/gc.md`](../specs/gc.md),
  [`../specs/remote.md`](../specs/remote.md),
  [`../specs/github-actions.md`](../specs/github-actions.md)
- **Decision:** [ADR 0006](../adr/0006-remote-tier-reapi-bazel-remote.md)
- **Status:** In progress

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
   `tinymold-cache` and `remote-e2e`, and `deploy/bazel-remote/`.
10. **README and bench**: a "Use in GitHub Actions" section, and a `ci`
    scenario in `bench/bench.py` with committed results.

## Completion checklist

- [ ] Steps 1-6 (local GC, stats, coverage rule)
- [ ] Steps 7-8 (remote tier)
- [ ] Step 9 (actions, CI, deployment)
- [ ] Step 10 (README, bench numbers)
