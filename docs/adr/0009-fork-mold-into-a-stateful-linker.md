# 9. Fork mold into a stateful linker

- **Status:** Accepted
- **Date:** 2026-10-15

## Context

The architecture spec listed "a new linker" as a non-goal: tinymold would
configure existing linkers, skip links it can prove redundant, and contribute
incremental linking to wild or mold. That rested on two facts from
[`linkers.md`](../research/linkers.md): mold already runs full links close to
the speed of copying the output, and wild had incremental linking on its
roadmap.

A source read of mold 3.0 and Bun
([`mold-and-bun.md`](../research/mold-and-bun.md)) changed the picture in
three ways:

- **mold is now Rust and MIT.** 3.0 is about 92k lines of Rust in a cargo
  workspace, so it can be built inside tinymold's workspace with no C++
  toolchain. Its Mach-O backend is substantial (about 30k lines, 668 tests),
  which matters on macOS, where tinymold's benchmarks run.
- **mold keeps no state between links.** Every link opens and parses every
  member of every archive, re-hashes every mergeable section and rewrites the
  whole output. In a Rust edit loop almost all of those inputs (std and every
  dependency rlib) are byte-identical to the previous link.
- **Bun shows the techniques that fit.** Content-addressed cache entries with
  an in-entry options hash and version, binary formats read with one call and
  a cast, and patching an executable's segments instead of rebuilding it.

Racing mold on cold links remains a bad bet: its ablation study finds every
pass already parallel and no single dominant cost. The opening is a linker
that does less work on the *second* link, and nobody upstream is building that
for mold today.

## Decision

- tinymold builds its own linker by **forking mold** at commit
  [`c522789`](https://github.com/rui314/mold/tree/c522789) (2026-10-11). It
  does not write a linker from scratch.
- The fork lives in the workspace as these crates, each a renamed mold crate:

  | tinymold crate | mold crate |
  |---|---|
  | `tinymold-link-common` | `mold-common` |
  | `tinymold-link-elf` | `mold-elf` |
  | `tinymold-link-macho` | `mold-macho` |
  | `tinymold-link-elf-x86_64` | `mold-elf-x86_64` |
  | `tinymold-link-elf-arm64` | `mold-elf-arm64` |
  | `tinymold-link-macho-arm64` | `mold-macho-arm64` |
  | `tinymold-link-macho-x86_64` | `mold-macho-x86_64` |
  | `tinymold-link` (binary `tinymold-link`) | `mold` (`cli/`) |

  Only these four targets are kept; the other 18 ELF targets can be imported
  again from upstream when someone needs them.
- **Source stays verbatim.** Dependencies are renamed with cargo's
  `package = "..."` key, so the code still says `mold_common::` and upstream
  commits apply with few conflicts. Every local change is listed in
  `crates/tinymold-link/FORK.md` together with the upstream commit it is based
  on.
- **License.** The forked crates keep `license = "MIT"` and mold's `LICENSE`
  file, inside the GPL-3.0 workspace. MIT is compatible with GPL-3.0, and
  keeping MIT on the fork lets changes flow back upstream.
- **Lints.** mold uses `unsafe` throughout (mmap, raw output buffers,
  syscalls), each use with a `// SAFETY:` comment from upstream. The forked
  crates do not opt into `[lints] workspace = true`, which forbids
  `unsafe_code`; each declares its own lint table that allows it. This is done
  in its own commit, as the repository guidelines require for relaxing a lint.
  New code tinymold adds outside the fork's seams stays in crates that keep the
  workspace lints.
- **Behaviour.** With no cache configured, `tinymold-link` must produce the
  same output as mold at the fork point. State that persists across links is
  added in phases ([spec](../specs/linker.md), [plan](../plans/linker.md)):
  a per-input parse cache in the tinymold CAS first, then a merge-fragment
  cache, a warm daemon, and append linking onto a prelinked dependency image.
- **Upstream.** Changes that make sense for mold in general (an N-slot jobs
  governor, Rust-aware archive handling, hooks for a cache) are offered to
  mold.

## Consequences

- The non-goal changes from "a new linker" to "a linker written from scratch,
  or one that competes with mold on cold links".
  [`architecture.md`](../specs/architecture.md) and the research synthesis
  are updated to match.
- tinymold owns a large body of code it did not write. Bugs in relocation
  handling, TLS or code signing are now tinymold's to triage. The cost is kept
  down by keeping the fork verbatim, but merging upstream becomes a recurring
  job.
- Workspace build time and `cargo test` time grow. Four target crates instead
  of 22, and mold's shell test suite behind a feature, keep that bounded; the
  growth is measured in the PR that imports the fork.
- The forked crates are exceptions to the house rules: no workspace lints,
  `unsafe` allowed, `_exit` on errors, process-wide statics. A warm daemon
  (Phase L4) needs per-link arenas, `Result` errors and a non-global thread
  pool, which are large changes to upstream code and are deferred until the
  parse cache has proved that persistent state pays off.
- Phase 7's "contribute incremental linking to wild" is replaced by the fork's
  own phases L4-L6. Upstreaming remains a goal, but to mold.
- If the measurements in phase L1 show that parsing and merging of unchanged
  inputs is a small share of a Rust link, the cache phases stop and the fork
  is kept only as a vehicle for link-skip, the governor and macOS Mach-O
  linking, or removed.
