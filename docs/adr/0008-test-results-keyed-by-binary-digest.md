# 8. Test results are keyed by the test binary's digest

- **Status:** Accepted
- **Date:** 2026-10-15

## Context

The wrapper caches compiles, but a warm `cargo test` still runs every test.
To skip a test run, tinymold has to decide which tests a change can affect.
There are three candidate units of invalidation:

- **Source files.** Map each test to the files it "uses" and rerun it when
  one changes. Rust has no reliable file-level map: generics, macros,
  inlining and trait impls cross files and crates freely.
- **Crate interfaces.** Rerun a test only when the public interface of a
  crate it uses changes. [ADR 0007](0007-no-interface-hashing-in-the-wrapper.md)
  found no sound interface oracle on stable rustc, and behaviour changes
  live in bodies, not interfaces, so an interface key would be wrong
  anyway.
- **The linked binary.** A test binary contains every piece of code the test
  can execute, apart from system libraries and dynamic libraries loaded at
  run time. Its digest changes exactly when that code changes, and the
  wrapper already records it in the test unit's ActionResult.

Go's test cache uses the same unit: the test binary's content ID plus the
files and env vars the test read
([fast-tools](../research/fast-tools.md)).

## Decision

- A recorded test result is keyed by the test binary's content digest, the
  arguments, the host, and the test's run-time inputs, as specified in
  [`test-cache.md`](../specs/test-cache.md).
- Only passing runs are recorded. A failure always runs again.
- Run-time file inputs default to the whole package tree, plus files the
  package declares, because reads cannot be traced portably today. This
  over-invalidates on purpose: a miss costs a test run, a wrong hit hides a
  regression.
- Selecting individual tests inside a changed binary from coverage data is
  not the default. It has known soundness gaps (data changes are not
  executed code; dependencies are not instrumented), so it stays an opt-in
  research item with its own ADR.
- Binaries that load workspace dynamic libraries at run time, bench runs and
  coverage runs are never cached in v1.

## Consequences

- The question "which tests did this edit affect" needs no new analysis: L1
  keys decide which binaries change, and the binary digest says whether they
  did.
- A rebuild that produces a byte-identical binary replays every result in
  it, which also covers comment and formatting edits that leave codegen
  unchanged.
- Any edit under a package's root reruns that package's test binaries, even
  when the binary is unchanged. Excluding compile inputs from the package
  tree, or tracing reads, would remove that cost; both are open questions in
  the spec.
- Results stay machine-local, since `PATH`, `HOME` and the host are keyed.
- Flaky tests that pass once are replayed as passes until an input changes.
  That matches Go's behaviour; `cache = false` opts a package out.
