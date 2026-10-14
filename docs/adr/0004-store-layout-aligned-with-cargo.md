# 4. Store layout aligned with cargo's proposed build cache

- **Status:** Accepted
- **Date:** 2026-10-14

## Context

Cargo is designing its own shared build cache
([cargo#5931](https://github.com/rust-lang/cargo/issues/5931),
[cargo#17453](https://github.com/rust-lang/cargo/issues/17453)). The
September 2026 proposal stores entries at
`<build-cache>/entries/<pkg>/<build-unit-hash>`. Each entry points into a
content store at `<build-cache>/content/`. Entries are inserted by hardlink,
and the manifest is written atomically. The prototype built about 20 projects
in 6m46s, against 12m59s on stable
([research](../research/compilers-2026.md#2-cargos-shared-build-cache-is-being-designed-now)).

That proposal covers non-local crates only. tinymold also caches several
things it leaves out:

- workspace crates across worktrees;
- link outputs and test binaries;
- build-script runs.

tinymold's two-level cache ([ADR 0003](0003-two-level-content-addressed-cache.md))
already has the same shape. What is undecided is how the action cache is laid
out on disk.

## Decision

- The action cache lives beside the CAS objects, as
  `entries/<pkg>/<action-key hex>`. Each entry is one ActionResult manifest
  that names objects under `objects/`. `<pkg>` is the crate name, or `_link`
  for link actions. It exists for listing and garbage collection only, never
  for lookup correctness.
- The manifest is the commit point. It is written to `tmp/` and renamed into
  place only after every object it names is present.
- The CAS keeps its existing `objects/<2 hex>/<62 hex>` layout and its
  reflink → hardlink → copy materialization.
- Cargo's build-dir layout v2 (`build/<pkg>/<hash>/`) is treated as one
  restorable unit per directory where it is in use.

## Consequences

- If cargo ships its cache, tinymold can import its entries or hand entries
  to it with a mechanical translation, because the shapes match.
- tinymold keeps what cargo's design lacks: path-normalized keys for
  workspace crates, cached link outputs, and build-script outputs.
- Grouping entries by package keeps directories small and makes per-crate
  garbage collection and `tinymold explain` listings cheap.
- If cargo's final layout differs, only the translation changes. tinymold's
  keys and objects do not.
