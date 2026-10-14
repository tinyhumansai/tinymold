# 7. No interface hashing in the wrapper on stable rustc

- **Status:** Accepted
- **Date:** 2026-10-15

## Context

The edit-loop plan ([`edit-loop.md`](../plans/edit-loop.md)) proposed
"RDR-lite": hash a rebuilt library's interface rather than its `.rmeta`
bytes, so that a private-body or comment edit lets every dependent hit the
cache. The research catalogue rated it high impact and noted it might need
rmeta decoding or a patched rustc
([rustc-pipeline §10, idea 11](../research/rustc-pipeline.md#10-ideas-tinymold-could-adopt-or-build-impact--difficulty)).
Roadmap Phase 7 asks for a time-boxed spike that ends in an ADR. This is
that ADR.

### The spike

Three versions of a library `up` were compiled with cargo's dev flags
(`-C debuginfo=2 -C split-debuginfo=unpacked`, rustc 1.99, aarch64-apple-darwin):

- the original;
- the same with a comment added at the top;
- the same with one private function body changed.

A dependent `down` was compiled against each version, and a binary was linked
from the original `down` and each newer `up`.

| Compared with the original | `up`'s `.rmeta` | `down`'s object code | `down`'s `.rmeta` | Link old `down` + new `up` |
|---|---|---|---|---|
| Comment at the top | differs | identical | differs | rustc refuses: E0460 |
| Private body changed | differs | identical | differs | rustc refuses: E0460 |

E0460 is "found possibly newer version of crate `up` which `down` depends
on".

- Every dependent records each upstream crate's hash (the SVH) in its own
  metadata.
- The SVH covers spans and every body, so any edit changes it.
- rustc checks the recorded hash whenever it loads `down`.
- The dependent's machine code did not change. Its metadata cannot be reused.

### What it would save

Measured on ripgrep (darwin arm64, 18 CPUs) with a private-body edit in
`grep-searcher`, which `grep-printer`, `grep` and `rg` depend on:

| Unit | Time |
|---|---:|
| `grep_printer` | 134-147 ms |
| `grep` | 26 ms |
| `rg` (compile and link) | 378-395 ms |
| Whole rebuild (wall) | 0.69-0.72 s |

The dependents are about three quarters of that rebuild, so the prize is real.

## Decision

The wrapper does not serve a dependent across an upstream change to its
`.rmeta`. Specifically:

- **Not for libraries.** A dependent library's metadata must name the new
  upstream SVH, which only rustc can write. Restoring the old one breaks the
  next crate that loads it with E0460. Patching the SVH inside an unstable,
  versioned metadata format would make every rustc release a potential
  miscompile.
- **Not for binaries, for now.** A binary or test has no metadata consumers,
  and its own objects were identical. In principle the wrapper could relink
  cached objects against the new upstream without running rustc. That needs
  an oracle for "nothing this crate's codegen can see has changed". Stable
  rustc offers none, and a source-level oracle would have to treat all of
  these as interface:
  - generic, `#[inline]` and `const fn` bodies;
  - bodies returning `impl Trait`, and `async fn` bodies, whose auto traits
    leak;
  - `impl` blocks and macro invocations inside bodies, which can define
    items visible crate-wide;
  - exported `macro_rules!`;
  - spans that reach a dependent through monomorphized panics and debuginfo;
  - with optimization, every small function rustc may inline across crates.

  Getting any of these wrong is a silent miscompile. That is the one failure
  the cache must not have
  ([AGENTS.md, cache correctness](../../AGENTS.md#cache-correctness)).

Early cutoff stays what it is: a dependent hits when the upstream's `.rmeta`
bytes are unchanged. That happens for edits that keep every span, such as a
comment or item appended at the end of a file.

## Consequences

- Body-only edits keep rebuilding dependents through rustc's own incremental
  compilation, which already reuses their unchanged objects.
- The work went where the spike showed waste instead. With early cutoff, a
  hit's time was almost all restore: placing hundreds of objects that were
  already in the out-dir. Restore now keeps outputs that are still in place
  (`docs/specs/rustc-wrapper.md`).
- Revisit when rustc offers an interface hash or an RDR mode
  ([the "Relink, don't rebuild" goal](https://goals.rust-lang.org/2025h2/relink-dont-rebuild.html),
  proof of concept [rust-lang/rust#143249](https://github.com/rust-lang/rust/pull/143249)). A crate hash
  that excludes private bodies and spans would make library dependents
  cacheable with no change to the key design: the extern digest would simply
  stop changing.
