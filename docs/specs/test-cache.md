# Affected work: test results, test edits and scoped flags

- **Status:** Draft
- **Owner:** Steven Enamakel
- **Date:** 2026-10-15
- **Builds on:** [`rustc-wrapper.md`](rustc-wrapper.md),
  [`cache-key.md`](cache-key.md),
  [ADR 0007](../adr/0007-no-interface-hashing-in-the-wrapper.md),
  [ADR 0008](../adr/0008-test-results-keyed-by-binary-digest.md)

## Problem

tinymold caches rustc invocations, so a warm `cargo test` compiles nothing.
It still runs every test, even in binaries whose bytes did not change.
Three questions decide how much more work a cache can skip:

1. When logic changes in one place, which tests can observe it?
2. When a test changes, which other cache entries does that invalidate?
3. Which flag changes re-key work that did not actually change?

The answers must be sound. A test result replayed for code it never ran
against hides a regression, and that is worse than running the test again
([cache correctness](../../AGENTS.md#cache-correctness)).

## Goals and non-goals

Goals:

- Replay a passing test run when nothing it can observe has changed.
- Rerun exactly the test binaries whose code or run-time inputs changed.
- Keep dependency units on one key across `check`, `clippy`, `build`, `test`
  and `llvm-cov`, wherever the flags provably do not change their output.

Non-goals for this spec:

- running, scheduling or reporting tests ourselves (cargo and nextest do);
- a coverage tool, a lint tool or a test harness;
- caching failures, benchmarks or doctests;
- sharing test results through the remote tier;
- selecting individual tests inside a binary from coverage data (an opt-in
  research item, [below](#later-test-impact-analysis)).

## The model: three layers of "what changed?"

```text
source edit
  │
  ▼  L1  unit key (exists)       pre-key: args + upstream artifact digests
  │                              full key: + every dep-info input's digest
  │      only units whose dependency closure contains the edit miss
  ▼  L2  binary identity         the test executable's content digest,
  │                              already recorded in the unit's ActionResult
  │      a rebuild that yields identical bytes stops here (early cutoff)
  ▼  L3  test key (new)          binary digest + arguments + run-time inputs
         a recorded pass is replayed; anything else runs
```

### Which tests can observe a logic change

A test's outcome is a function of two things:

- **The code linked into its binary.** The test unit's pre-key holds the
  content digest of every `--extern` artifact it links, and each of those was
  keyed the same way. A logic change anywhere in the closure changes an
  upstream digest, so the test unit misses, relinks, and produces a binary
  with a new digest. A crate the binary does not link cannot change its
  bytes. **The binary digest is therefore an exact and sound answer to "is
  this test binary affected", at no extra cost:** the wrapper already has it.
- **What it reads at run time:** fixture files, env vars, the clock, the
  network. Nothing compile-time sees these. This spec handles them with a
  conservative default plus declared inputs, and treats anything else
  (time, network, randomness) as the test's own nondeterminism, as Go does.

Finer selection, within one binary, needs per-test coverage, and it has
soundness gaps (statics, vtables, uninstrumented code). It is opt-in and
later.

### What a test change invalidates

Tests are leaves: nothing compiles against a test binary. Measured with
rustc 1.99 (scratch probe, no debuginfo):

- Editing a file reached only through `#[cfg(test)] mod test;` does **not**
  appear in the library unit's dep-info. The library, and every dependent,
  keeps its key; only the library's own `--test` unit recompiles. This is
  why this repository keeps unit tests in a separate `test.rs`.
- An inline `#[cfg(test)] mod tests { … }` lives in the same file as the
  code. Editing it changes that file's digest, so the library unit misses
  too. Its output may still be byte-identical, and then early cutoff on
  `.rmeta` stops the change there. A test module at the bottom of the file
  keeps every span above it, which makes that more likely.
- A comment-only edit to the test file produced a byte-identical test
  binary, so L2 stops the change before L3: the recorded results replay.

## L3: the test result cache

### Hook

Cargo's `runner` setting is a stable hook, like `RUSTC_WRAPPER`. Cargo runs
`<runner> <binary> <args…>` with the package root as the working directory
(verified: `target/debug/deps/<name>-<hash> --quiet`, cwd = the package
root). nextest honours the same setting.

```toml
# .cargo/config.toml
[target.'cfg(all())']
runner = ["tinymold", "run-test"]
```

An existing runner is kept by naming it: `["tinymold", "run-test", "--via",
"qemu-aarch64"]`. Its digest joins the key.

### Which invocations are cacheable

`tinymold run-test` caches an invocation only when all of these hold.
Everything else runs unchanged, with the same argv, env and exit code.

- The binary has a **test-binary record**. When the wrapper compiles or
  restores a unit built with `--test`, it writes `tests/bin/<binary digest>`:
  the package root normalized to `${WS}`, the crate name, and whether the
  unit links a dynamic library from `${WS}` or `${OUT}`. A binary tinymold
  did not build has no record and is never cached. This also keeps `cargo
  run` and examples out.
- The arguments do not contain `--bench`. `cargo bench` runs the same kind
  of binary.
- `LLVM_PROFILE_FILE` is unset. A replayed run would not write the `.profraw`
  coverage needs; caching profiles is a later slice.
- The record says the binary links no workspace dynamic library: those are
  loaded at run time and their bytes are not in the binary.
- `TINYMOLD_TEST_CACHE` is not `0`, and the package does not set
  `cache = false` (below).

### The test key

The BLAKE3 digest of these fields in order, each length-prefixed with
`tinymold_hash::Hasher`, like the rustc key:

1. **Schema tag:** `tinymold/test/v1`.
2. **The binary:** its content digest, read through the stat cache, so an
   unchanged binary is not rehashed.
3. **The via-runner:** its path and digest, or empty.
4. **The arguments, in order.** Only `--color` and its value are dropped.
   Filters, `--exact`, `--include-ignored`, `--format`, `--quiet` and
   `--test-threads` stay: they change what runs or what is printed.
5. **Not the working directory.** Cargo runs tests in the package root,
   which normalizes to `${CWD}` in every worktree, so it adds nothing; its
   contents are the package tree below.
6. **The host:** OS, architecture and kernel release. Tests call system
   libraries that the binary digest does not cover.
7. **The package tree:** a Merkle digest of `(path, content digest)` for
   every file under the package root, except the target directory, `.git`,
   and nested packages. It is the conservative default for fixtures: a test
   reading `tests/data/*.json` reruns when that file changes. Digests come
   from the stat cache, so only changed files are read.
8. **Declared inputs:** the files matched by the package's declared globs,
   with the same Merkle digest.
9. **The environment** the test process receives, sorted by name, with
   absolute paths in values rewritten to `${WS}`, `${CARGO_HOME}` and
   `${OUT}`, minus a pinned ignore list: `TERM`, `COLORTERM`, `CLICOLOR`,
   `CLICOLOR_FORCE`, `NO_COLOR`, `COLUMNS`, `LINES`, `SHLVL`, `_`, `OLDPWD`,
   `PWD`, `CARGO_MAKEFLAGS`, `MAKEFLAGS`, `MFLAGS`, `TERM_PROGRAM`,
   `TERM_PROGRAM_VERSION`, `TERM_SESSION_ID`, `SSH_*`, and `TINYMOLD_*`.
   The list is part of the encoding: changing it is a schema bump.

`PATH` and `HOME` stay in. They make results machine-local, which is the
v1 scope anyway.

### Declared inputs

```toml
[package.metadata.tinymold.test]
inputs = ["../fixtures/**", "../schema.sql"]  # read outside the package root
cache = false                                 # never cache this package's tests
```

Globs are relative to the package root. A glob that escapes every known root
makes the package uncacheable, with an explained reason, like an input
outside every root does for rustc.

### Hit, miss and record

- **Hit:** write the stored stdout and stderr, exit 0. With
  `TINYMOLD_REMARKS=1`, add one `tinymold: cached test result <key>` line
  on stderr; otherwise print nothing extra, so parsers of libtest JSON and
  nextest's output see the original bytes.
- **Miss:** run the binary (through the via-runner), stream its output
  through while capturing it, and return its exit status.
- **Record:** only on exit 0 with no signal. Store stdout and stderr in the
  CAS, then write `tests/run/<test key>` last: exit code, both digests,
  duration, and tinymold version. Line-based text like the other records,
  pinned in tests, listed by the catalog for `tinymold gc`.

Failures, timeouts, signals and `--list` errors always run again. `--list`
itself is cacheable: nextest lists every binary before running it.

### Granularity

- `cargo test` runs one process per binary: a hit replays the whole binary.
- nextest runs one process per test (`<binary> --exact <name> …`), so the
  same key caches each test on its own, with no extra code.

## Flag scoping

These keep dependency units on one key across tools.

1. **Lint flags under `--cap-lints allow`.** Cargo passes `--cap-lints
   allow` to every non-path dependency (verified in a `cargo build -v` log).
   rustc then ignores `-A`, `-W`, `-D`, `-F` and their long forms. Verified:
   adding `-D warnings -W unused -F deprecated` left the `.rlib` and `.rmeta`
   byte-identical. Such units drop those flags from the pre-key, so
   `RUSTFLAGS="-D warnings"` in CI stops re-keying every registry
   dependency. `--force-warn` is kept. Without `--cap-lints allow`, lint
   flags stay, because they change diagnostics and the exit status.
2. **Coverage.** `cargo llvm-cov` 0.8 already instruments only workspace
   crates. It sets itself as `RUSTC_WRAPPER`, chains to a pre-existing one
   (tinymold), and adds `-C instrument-coverage --cfg=coverage` only for
   workspace crate names. Dependencies are therefore keyed as plain builds
   and hit across `test` and `llvm-cov`. No change is needed; the
   [GitHub Actions spec](github-actions.md#coverage-clippy-and-incremental)
   says so. A hand-written `RUSTFLAGS=-C instrument-coverage` does re-key
   every dependency; `tinymold doctor` should point at `cargo llvm-cov`.
3. **Clippy.** `cargo clippy` uses `RUSTC_WORKSPACE_WRAPPER=clippy-driver`,
   so dependencies are ordinary `--emit=metadata` units shared with
   `cargo check`. Workspace crates reach tinymold as `clippy-driver` calls,
   which run unchanged today. Caching them means keying on the
   `clippy-driver` digest, `clippy.toml`, and `CLIPPY_ARGS`, and replaying
   the diagnostics. Whether clippy records `clippy.toml` and `CLIPPY_ARGS`
   in dep-info must be verified before relying on dep-info for them.

## Links

Link outputs are already cached when a unit hits: the wrapper stores bin and
test units whole, rustc and link together. The remaining link work, link-skip
when a unit misses but every linker input is unchanged, is roadmap Phase 4
and is not changed here. In this model it is a second way to reach L2: the
same binary digest, without running the linker.

## Later: test impact analysis

`tinymold test --affected` would rerun only the tests, within a changed
binary, that executed changed code. It needs a per-test coverage map
(nextest plus one `LLVM_PROFILE_FILE` per test) from functions to the source
regions they cover. A test reruns when a function it executed has a changed
region, or when it is new, or when its L3 inputs changed. Known gaps:
changes to statics, consts, vtables and layouts are data, not executed
functions; dependency code is not instrumented. So any dependency or
`static`/`const` change reruns the whole binary, the mode is off on `main`
and in CI, and a verify mode reruns a sample of skipped tests. It gets its
own spike and ADR before any code.

## Invariants

- A test result is replayed only for the same binary bytes, arguments,
  host, package tree, declared inputs and environment.
- Only passes are recorded. A failure is never replayed.
- The runner never changes the outcome of a test it runs: same argv, env,
  working directory, output bytes and exit status as without tinymold.
- Any internal failure runs the test unchanged.
- A binary without a test-binary record, a bench run, or a coverage run is
  never cached.
- The record is written after its output objects, so a reader that finds
  one can always replay it.

## Acceptance criteria

- A golden test pins the test-key encoding, including the env ignore list.
- Changing any keyed field changes the key; changing `--color` or an
  ignored env var does not. Table-driven tests cover both.
- On the corpus, a second `cargo test` with no edits runs zero test
  binaries, and its output matches the first run's apart from timings.
- Editing one library reruns exactly the test binaries that link it,
  checked against `cargo tree` on ripgrep and tinymold.
- Editing a fixture under `tests/` reruns that package's binaries only.
- A failing test fails again on the next run, without being replayed.
- `cargo bench`, `cargo run` and `cargo llvm-cov` behave exactly as without
  the runner.
- `RUSTFLAGS="-D warnings" cargo build` after `cargo build` restores every
  registry dependency from the cache.
- `bench/bench.py` numbers, before and after, for a warm second `cargo test`
  and a one-line edit, on a named machine.

## Open questions

- Doctests: rustdoc runs them without a runner. Is a `RUSTDOC` wrapper worth
  it, or are doctests a separate spec?
- Tracing reads at run time (Linux fanotify or seccomp, macOS
  `DYLD_INSERT_LIBRARIES` on non-hardened binaries) would replace the
  package-tree default with exact inputs. Is it portable enough?
- Can the package-tree digest skip files that are compile inputs of the
  binary, which its digest already covers? That would stop a source comment
  from rerunning tests whose binary did not change.
- Caching `.profraw` so coverage runs hit too, which needs the profile file
  pattern (`%p`, `%m`) expanded on replay.
- Remote sharing of results, once the key covers enough of the host.
