# Plan: affected work — test results, test edits and scoped flags

- **Spec:** [`../specs/test-cache.md`](../specs/test-cache.md)
- **Decision:** [ADR 0008](../adr/0008-test-results-keyed-by-binary-digest.md)
- **Roadmap:** slices 1–5 belong to Phase 4 (test builds and links), slice 6
  to Phase 7 (research bets).
- **Status:** Not started.

Each slice is its own pull request, written test first: a failing test for
the behaviour, then the smallest change that passes it. Every slice ends
with the four contract commands green.

## Facts checked before writing the spec

Measured on darwin arm64 with rustc 1.99 and cargo-llvm-cov 0.8.7, in
scratch crates:

- cargo calls the runner as `<runner> <abs path to deps/<name>-<hash>>
  <args…>` with the package root as the working directory, and accepts
  `runner` under `[target.'cfg(all())']`; `cargo bench` passes `--bench`.
- cargo passes `--cap-lints allow` to a registry dependency and not to the
  workspace crate; `-D warnings -W unused -F deprecated` under `--cap-lints
  allow` left `.rlib` and `.rmeta` byte-identical. (Slice 1 later found
  that cargo ≥ 1.88 renames the unit anyway.)
- `cargo llvm-cov` wraps rustc itself, chains to the existing
  `RUSTC_WRAPPER`, and instruments only workspace crate names.
- A file reached only through `#[cfg(test)] mod test;` is absent from the
  library's dep-info; a comment appended to it left the test binary
  byte-identical.

## Slice 1: lint flags under `--cap-lints allow` leave the key (dropped)

Built test first, then dropped before merging. Branch
`lint-flag-scoping` holds the commits.

- Key tests and a wrapper end-to-end test (`a_capped_dependency_hits_across_lint_flags`)
  passed. The end-to-end test failed with the filter disabled.
- With real cargo, a registry dependency hit across
  `RUSTFLAGS="-D warnings"` on cargo 1.84.1. It still missed on cargo
  1.88, 1.93 and 1.99, because those versions hash `RUSTFLAGS` into
  `-C extra-filename`, which is in the output bytes
  ([spec](../specs/test-cache.md#flag-scoping)).
- Shipping it would have bumped the schema to `tinymold/rustc/v3`,
  orphaning every entry, for a gain only on cargo older than tinymold's own
  rust-version.

## Slice 2: the test key

- New module `crates/tinymold-key/src/test_key/` (`mod.rs`, `types.rs`,
  `test.rs`): `TestKeyInputs` (binary digest, via-runner, args, cwd, host,
  package-tree digest, declared-input digest, env) and `test_key`. Pure
  functions: the caller supplies every digest.
- `normalize_test_args` drops `--color` in both spellings and keeps the
  rest in order.
- `ENV_IGNORED`: the pinned ignore list, with `SSH_*` and `TINYMOLD_*` as
  prefixes; values rewritten through the existing `Roots`.
- Tests: a golden vector; one table-driven test that each keyed field
  changes the key; one that `--color` and each ignored variable do not.

## Slice 3: the test-binary record and `tinymold run-test`

- `crates/tinymold-wrap/src/record/`: a `TestBinary` record at
  `tests/bin/<binary digest>` (package root as `${WS}`-relative path,
  crate name, links-workspace-dylib flag) and a `TestRun` record at
  `tests/run/<test key>` (exit code, stdout and stderr digests, duration,
  tinymold version). Encodings pinned in `record/test.rs`.
- `crates/tinymold-wrap/src/compile/`: after a hit or an ingest of a unit
  built with `--test`, write the `TestBinary` record for its executable.
- New module `crates/tinymold-wrap/src/runner/`: decide cacheability (record
  present, no `--bench`, no `LLVM_PROFILE_FILE`, no workspace dylib, not
  opted out), compute the package-tree digest with the `stat` module,
  read `[package.metadata.tinymold.test]`, look up, replay or run. Run
  with `exec`, teeing output; record only exit 0 without a signal. Any
  internal error runs the binary unchanged.
- `crates/tinymold-wrap/src/catalog/`: list both record kinds and their
  objects, so `tinymold gc` sweeps them ([`gc.md`](../specs/gc.md)).
- `crates/tinymold/src/cli/`: the `run-test [--via <program>] <binary>
  <args…>` subcommand; `TINYMOLD_TEST_CACHE=0`; remarks under
  `TINYMOLD_REMARKS=1`.
- Tests, each written to fail first:
  - `a_passing_run_is_replayed_without_running_the_binary`;
  - `a_failing_run_is_never_recorded`;
  - `a_changed_binary_runs_again`;
  - `a_changed_fixture_in_the_package_runs_again`;
  - `a_declared_input_outside_the_package_runs_again_when_changed`;
  - `a_bench_run_passes_through`;
  - `a_coverage_run_passes_through`;
  - `a_binary_without_a_record_passes_through`;
  - `output_and_exit_status_match_an_unwrapped_run`;
  - integration test in `crates/tinymold/tests/`: a two-crate workspace
    under `cargo test` with the runner; edit the library and assert that
    exactly the binaries linking it ran again.

## Slice 4: clippy-driver calls

- Verify, in a scratch crate, whether `clippy-driver` writes `clippy.toml`
  and `CLIPPY_ARGS` into dep-info. Record the result in the spec.
- If it does: treat `clippy-driver` as a cacheable compiler. Key on its
  `-vV` output and digest as for rustc, and replay diagnostics. If it does
  not: add `clippy.toml` (searched as clippy does) and `CLIPPY_ARGS` to the
  pre-key explicitly.
- Tests: a lint change in `clippy.toml` misses; an unchanged second
  `cargo clippy` runs no `clippy-driver`.
- **Done** with the edit-rebuild work ([`edit-rebuild.md`](edit-rebuild.md)).
  clippy-driver does write `clippy.toml`, `Cargo.toml`, `CLIPPY_ARGS` and
  `CLIPPY_CONF_DIR` into dep-info, so the full key covers them. A
  configuration file that does not exist yet is in no dep-info, so the
  pre-key also names the file clippy would find, or `none`
  (`tinymold-wrap/src/driver`, `rustc-wrapper.md`).

## Slice 5: measure

- `bench/bench.py`: a `pipeline` benchmark — `cargo fmt --check`, `cargo
  clippy`, `cargo build`, `cargo test`, `cargo llvm-cov --no-report` — run
  cold, warm, and after a one-line library edit, with and without the
  runner. A `retest` case runs `cargo test` twice with no edit.
- Commit the result JSON and the `python3 bench/report.py` output; quote the
  before and after numbers in each slice's pull request.

## Slice 6: test impact analysis spike

- Time-boxed. Per-test profiles with nextest and `LLVM_PROFILE_FILE`,
  function-to-region maps from `llvm-cov export`, and a comparison against
  full reruns on the corpus's history to count missed failures.
- Ends in an ADR that accepts or rejects `tinymold test --affected`.

## Completion checklist

- [x] Slice 1: dropped, measured (cargo ≥ 1.88 re-keys by output name)
- [ ] Slice 2: test key
- [ ] Slice 3: records and `run-test`
- [x] Slice 4: clippy-driver caching
- [ ] Slice 5: pipeline and retest benchmarks
- [ ] Slice 6: impact-analysis ADR
