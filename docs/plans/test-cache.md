# Plan: affected work — test results, test edits and scoped flags

- **Spec:** [`../specs/test-cache.md`](../specs/test-cache.md)
- **Decision:** [ADR 0008](../adr/0008-test-results-keyed-by-binary-digest.md)
- **Roadmap:** slices 1–5 belong to Phase 4 (test builds and links), slice 6
  to Phase 7 (research bets).
- **Status:** Slices 2 and 3 implemented.

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
- Every argument is keyed in order, `--color` included: a hit replays
  the run's output, colors and all.
- `ENV_IGNORED`: the pinned ignore list, with `SSH_*` and `TINYMOLD_*` as
  prefixes; values rewritten through the existing `Roots`.
- Tests: a golden vector; one table-driven test that each keyed field
  changes the key, `--color` among them; one that each ignored variable
  does not.

## Slice 3: the test-binary record and `tinymold run-test`

- `crates/tinymold-wrap/src/test_record/`: a `TestBinary` record at
  `tests/bin/<binary digest>` (crate name; the file names of the dynamic
  libraries in the binary's directory that the compile linked against;
  `exes`, the package binaries cargo passed as `CARGO_BIN_EXE_<name>`; and
  `out_dirs`, the build script's `OUT_DIR`, both as `${PROFILE}/…` or
  `${OUT}/…` paths) and a `TestRun` record at `tests/run/<test key>` (the
  stdout and stderr digests and the `order` of their interleaving: just
  passes are recorded, so the exit code is always 0). Encodings pinned in `test_record/test.rs`.
- `crates/tinymold-wrap/src/compile/`: after a hit, a remote hit or a
  stored miss of a `--test` unit that links, `note_test_binary` writes the
  `TestBinary` record for its executable and remembers the executable's
  digest in the stat cache. No record for `-C prefer-dynamic`, a dynamic
  library linked from another directory, a shared library in a native
  search path inside the build, or a package binary or `OUT_DIR` outside
  the build.
- `crates/tinymold-wrap/src/package_tree/` and `declared/`: list the
  package tree, with digests from the stat cache, and read
  `[package.metadata.tinymold.test]` with a strict subset of TOML.
- New module `crates/tinymold-wrap/src/runner/`: decide cacheability
  (record present, no `--bench`, no `LLVM_PROFILE_FILE`, no input on
  stdin, not opted out), key the package tree, the declared inputs and
  every dylib, package binary and `OUT_DIR` the record names, then look
  up, replay or run. A miss runs the binary with an empty stdin, streaming
  its output through `exec` while capturing it, and records only exit 0
  without a signal, and only if nothing it may have read changed during
  the run. Any internal error runs the binary unchanged.
- `crates/tinymold-wrap/src/catalog/`: list both record kinds and the run
  records' objects, so `tinymold gc` sweeps them ([`gc.md`](../specs/gc.md)).
- `crates/tinymold/src/cli/`: the `run-test [--via <program>] <binary>
  <args…>` subcommand; `TINYMOLD_TEST_CACHE=0`; `test-hit`, `test-miss`,
  `test-pass` and `test-error` lines in the `TINYMOLD_LOG` log, which
  `tinymold stats` counts apart from compiles.
- Tests, each written to fail first:
  - `a_passing_run_is_replayed_without_running_the_binary`;
  - `a_failing_run_is_never_recorded`;
  - `a_changed_binary_runs_again`;
  - `a_changed_file_in_the_package_runs_again`;
  - `a_declared_input_outside_the_package_runs_again_when_changed`;
  - `a_changed_build_script_output_or_package_binary_runs_again`;
  - `bench_coverage_and_opted_out_runs_pass_through`;
  - `a_binary_tinymold_did_not_build_passes_through`;
  - `a_run_whose_stdin_may_carry_input_is_never_cached`;
  - `run_test_passes_arguments_and_exit_status_through`;
  - integration test in `crates/tinymold/tests/run_test.rs`
    (`cargo_test_reruns_exactly_the_binaries_an_edit_reaches`): a
    two-crate workspace under `cargo test` with the runner; edit the
    library and assert that exactly the binaries linking it ran again.

### Found while building slice 3

- **A deadlock between the runner and `main`.** `main` held
  `io::stdout().lock()` for the whole command, and the runner streams the
  test's output from worker threads, which waited on that lock forever.
  `main` now passes the unlocked `Stdout`. The end-to-end tests in
  `crates/tinymold/tests/run_test.rs` hung before the fix.
- **Declared inputs are paths, not globs.** There is no glob or TOML crate
  in the tree, and neither is worth its build time here: a directory
  stands for every file under it, and a strict reader handles the table.
- **Nested packages stay in the package tree.** A test may read one as a
  fixture; leaving them out would be a wrong hit.
- **A wrong hit, caught before merge.** An integration test that runs its
  package's binary (`CARGO_BIN_EXE_<name>`) does not change when that
  binary does, so editing a crate only the binary links replayed a stale
  pass (`an_integration_test_reruns_when_the_binary_it_runs_changes`
  reproduced it). The test-binary record now names those binaries and the
  build script's `OUT_DIR`, and their contents join the key. Found while
  designing the slice 5 benchmark on ripgrep, whose integration tests run
  `rg`.
- **Early cutoff at the binary.** An edit rustc folds away (`1` to
  `2 - 1`) left a dependent's test binary byte-identical, and its result
  was replayed, as the design intends.

## Slice 4: clippy-driver calls

**Status.** Built on the `clippy-cache` branch by another session, ahead
of this plan, together with unrelated work (outputs carried over between
compiles, benchmark changes). It keys `clippy-driver --version` and the
config file clippy's own lookup finds, or `none`, which covers a
`clippy.toml` created after a hit; clippy's dep-info already lists
`clippy.toml`, `Cargo.toml`, `CLIPPY_ARGS` and `CLIPPY_CONF_DIR`
(verified with clippy 0.1.99). Not duplicated here.

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

- `bench/bench.py retest` (done): `cargo test` for a project's package,
  both sides built through the wrapper into one `target/`, with and without
  the runner, taking alternate turns. Cases `again` (no edit) and
  `private_body` (a new one-line private edit before every measurement).
- A `pipeline` benchmark — `cargo fmt --check`, `cargo clippy`, `cargo
  build`, `cargo test`, `cargo llvm-cov --no-report` — remains, and waits
  for slice 4 to merge so clippy is measured cached.
- Commit the result JSON and the `python3 bench/report.py` output; quote the
  before and after numbers in each slice's pull request.

## Slice 6: test impact analysis spike

- Time-boxed. Per-test profiles with nextest and `LLVM_PROFILE_FILE`,
  function-to-region maps from `llvm-cov export`, and a comparison against
  full reruns on the corpus's history to count missed failures.
- Ends in an ADR that accepts or rejects `tinymold test --affected`.

## Completion checklist

- [x] Slice 1: dropped, measured (cargo ≥ 1.88 re-keys by output name)
- [x] Slice 2: test key
- [x] Slice 3: records and `run-test`
- [x] Slice 4: clippy-driver caching
- [ ] Slice 5: pipeline and retest benchmarks
- [ ] Slice 6: impact-analysis ADR
