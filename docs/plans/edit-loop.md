# Plan: the edit loop (roadmap Phase 5), first experiments

- **Spec:** the edit-loop goals in [`roadmap.md`](roadmap.md#phase-5-edit-loop)
  and the ideas in [`../research/compilers-2026.md`](../research/compilers-2026.md#10-ideas-ranked-for-tinymold)
- **Status:** First experiments done. One setting is adopted as a
  recommendation (`codegen-units = 64`). The structural levers (early cutoff,
  prelinked dependencies, hot patching) are next.

## Where an edit's time goes

These are the measurements the experiments were chosen from, on ripgrep
14.1.1 for a comment-only edit of `crates/core/main.rs` on darwin arm64.

**Wall time is 0.6-0.7 s**, split between rustc and cargo:

- **rustc for the `rg` crate is 0.42-0.53 s.** That comes from a per-call
  timing `RUSTC_WRAPPER`.
- **Cargo itself is the remaining ~0.15 s:** fingerprinting, the uplift and
  process start.

**Inside rustc,** from `-Ztime-passes`, added to the `rg` crate only so that
the edit stays incremental, out of 0.50 s in total:

| Phase | Time |
|---|---:|
| Code generation | 0.10 s |
| Of which LLVM passes | 0.06 s |
| Preparing the incremental session directory (hardlinking about 256 objects from the last session) | 0.08 s |
| Link | 0.075 s |
| Macro expansion, dep-graph and cache serialization, mono collection | ~0.08 s |
| Many small passes | the rest |

Code generation is only a fifth of an incremental edit. Settings that only
make LLVM faster cannot move the edit loop much, and they did not.

## Experiments

Variants were run with `bench/bench.py scenarios --variants …`, results in
[`bench/README.md`](../../bench/README.md), "Edit-loop experiments". These
runs are sequential on a shared machine, so load drift moves them by tens
of percent in either direction: they found no setting worth an A/B run
except `codegen-units`, but they do not show that the others have no effect.
Only the interleaved `ab` runs below support a conclusion.

| Variant | Result on the edit loop |
|---|---|
| `deps-nodebug`, `line-tables`, `nodebug` (debug-info levels) | Inconclusive: no consistent gain across projects and edit kinds; not A/B tested |
| `nightly` (1.98.0-nightly, 2026-06-29) | Slightly slower than stable 1.99 |
| `threads-8` (`-Zthreads=8`, the parallel front end) | Inconclusive against `nightly`: no consistent gain; not A/B tested. The front end is a small share of an edit |
| `cranelift` (`-Zcodegen-backend=cranelift` for every crate) | Inconclusive on edits; not A/B tested. It **breaks bat's build**: indexmap's `autocfg` probe fails under Cranelift, indexmap loses `std`, and `plist` no longer compiles. If tried again, it should apply to workspace crates only |
| `codegen-units` 16, 32, 64, 128 (default 256 with incremental) | Fewer units cut the session bookkeeping, but make each regenerated unit bigger; see below |

The Cranelift component does ship for aarch64-apple-darwin (installed with
`rustup component add rustc-codegen-cranelift-preview --toolchain nightly`),
which settles an open question in the research.

### Interleaved A/B on `codegen-units`

`python3 bench/bench.py ab --variant cgu-<n> --runs 7`: every edit is built by
both sides back to back, in alternating order, each side in its own target
directory. Results are in [`bench/README.md`](../../bench/README.md), "A/B".
Median change against the default (256 units):

| Units | Edit | ripgrep | bat | tinymold |
|---:|---|---:|---:|---:|
| 32 | comment / private body / public signature | −22% / **+15%** / −32% | −31% / −27% / −28% | – |
| **64** | comment / private body / public signature | **−14% / −13% / −21%** | **−16% / −17% / −15%** | +4% (6 ms) / −14% / −13% |
| 128 | comment / private body / public signature | −17% / −7% / **+22%** | −24% / −21% / −20% | – |

The 128-unit run ended at load 11.5, so its +22% may be partly noise.

- **64 is the only value with no meaningful regression.** 32 slows a
  private-body edit on ripgrep, where the regenerated unit gets too big, and
  128 slowed a public-signature edit.
- **Clean builds are neutral with 64.** ripgrep took 3.21 s against 3.40 s,
  bat was within noise, and incremental caches were 2-6% smaller.

**Recommendation:** add this to the workspace's `Cargo.toml` for the dev loop:

```toml
[profile.dev]
codegen-units = 64
```

The optimum likely depends on core count (this machine has 18). It is a
recommendation, not a tinymold default. A later `tinymold doctor --fix`
profile can apply it as a reversible change once it has been measured on
Linux.

## What's next for the edit loop

The remaining cost is spread across cargo, the incremental machinery,
codegen and the link, so the next gains have to remove work rather than tune
it:

1. **Early cutoff and interface hashing (RDR-lite).** A private-body edit in
   a library rebuilds dependents because its `.rmeta` changes.
   - The rustc wrapper already cuts off when the `.rmeta` bytes are
     identical. The bench's edits add a line, which shifts spans, so they
     never are.
   - A span-insensitive interface digest would let dependents hit.
2. **A prelinked dependency image and Roc-style append linking,** to shrink
   the 0.06-0.08 s link toward the size of the workspace code.
3. **Hot patching** (Subsecond-style) for `cargo run` loops, which skips the
   relink entirely.
4. **The cargo overhead** (~0.15 s) only goes away with a resident process,
   which is the daemon of Phase 6.
