# rustc cache key: ActionKey and ActionResult

- **Status:** Draft (to be accepted in the Phase 3 spec)
- **Owner:** Steven Enamakel
- **Date:** 2026-10-14
- **Implemented by:** `tinymold-key` (planned)

This spec defines the key tinymold computes for one rustc invocation and the
record it stores for that key. It adapts the draft schema in
[hashing-and-cache-server §6](../research/hashing-and-cache-server.md#6-proposed-cache-key-schema-for-one-rustc-invocation)
and the cacheability analysis in
[rustc-pipeline §5b](../research/rustc-pipeline.md#5b-cacheability-what-rustc-outputs-can-be-reused-and-what-breaks-reuse).
The overall model is in [ADR 0003](../adr/0003-two-level-content-addressed-cache.md)
and [architecture.md](architecture.md#the-two-level-cache).

## Problem

A cache hit must be indistinguishable from running rustc. So the key must
cover every input that can change the output bytes, diagnostics or exit
status. It must also exclude everything that does not, or hits never happen:
absolute paths, mtimes, terminal settings. Some inputs (source files pulled in
by `mod` and `include_str!`, env vars read by `env!`) are only known after
rustc has run once.

## Goals and non-goals

Goals:

- Equal keys imply interchangeable outputs.
- The same crate, at the same content, in two worktrees or on two machines
  produces the same key.
- Upstream changes that do not change upstream *output bytes* do not change
  downstream keys (early cutoff).
- Every component of a key can be printed, so a miss can be explained.

Non-goals: keys for whole cargo invocations, test results, or proc-macro
expansions. Those are separate schemas if and when they are built.

## Encoding

- The key is `BLAKE3-256(canonical_encoding(KeyInputs))`
  ([ADR 0002](../adr/0002-blake3-for-content-digests.md)).
- The canonical encoding is deterministic: fixed field order, sorted maps and
  sets, length-prefixed strings, no floats. Deterministic CBOR (RFC 8949
  §4.2) is the proposed format; the Phase 3 spec settles it. Whatever the
  format, a golden test pins the encoding of a fixed input to a fixed digest.
- Every key starts with a schema tag, `tinymold/rustc/v1`. Changing what goes
  into the key, or how, bumps the tag, which invalidates old entries instead
  of misreading them.

## Schema

```text
PreKeyInputs = {
  schema:            "tinymold/rustc/v1",
  tool: {
    rustc_digest:    BLAKE3(rustc binary) + `rustc -vV` (commit-hash, host, LLVM version),
    sysroot_digest:  Merkle digest of the target's std .rlib/.rmeta files,
    linker:          { path_rel, digest, version }        # only when this action links
  },
  target:            target triple, or BLAKE3(target-spec JSON) for custom targets,
  crate: {
    name, crate_types[], edition, emit[],
    root_src_rel:    crate root path relative to ${WS} or ${CARGO_HOME}
  },
  args_normalized:   every rustc argument in order, with paths rewritten to
                     ${WS}, ${CARGO_HOME}, ${SYSROOT}, ${OUT} and the
                     exclusions listed below removed,
  cfg_and_features:  sorted --cfg and --check-cfg values,
  externs:           sorted [(crate_name, BLAKE3(upstream .rmeta))],
                     or BLAKE3(upstream .rlib) when this action links,
  proc_macros:       sorted [(name, identity)], where identity is the ActionKey
                     that produced the dylib, or BLAKE3(dylib) when tinymold
                     did not build it (see "Proc-macro identity" below),
  build_script_out:  Merkle digest of OUT_DIR contents + the cargo: directives
}

KeyInputs = PreKeyInputs + {
  inputs:            sorted [(path_rel, BLAKE3(content))] for every file the
                     recorded dep-info lists (include_str!, include_bytes!,
                     and -Zbinary-dep-depinfo extras included),
  env:               sorted {name: value} for every env var the recorded
                     dep-info lists as env-dep, plus CARGO_PKG_*,
                     CARGO_MANIFEST_DIR (normalized), CARGO_CRATE_NAME,
                     OUT_DIR (normalized), RUSTC_BOOTSTRAP, SOURCE_DATE_EPOCH,
                     CARGO_CFG_*
}

ActionResult = {
  outputs:           [(kind, path_rel, BLAKE3, size, exec_bit)]
                     kind ∈ rlib | rmeta | bin | dylib | cdylib | d | dwo | dSYM-dir,
  stdout_digest, stderr_digest,          # diagnostics replayed with ${WS} re-expanded
  exit_code,
  recorded_input_paths: [path_rel],      # feeds the next pre-key lookup
  recorded_env_names:   [name],
  producer:          { host, tinymold_version, created_at },
  verified:          bool
}
```

Profile settings (opt-level, debuginfo, LTO, codegen-units, panic strategy,
debug-assertions, overflow-checks, split-debuginfo, strip) reach rustc as
arguments and are therefore covered by `args_normalized`. `RUSTFLAGS` and
`[lints]` arrive the same way, which matters because cargo excludes
`RUSTFLAGS` from `-C metadata`: two builds with the same artifact filename can
differ in flags.

## Proc-macro identity

A proc-macro dylib's bytes are not a stable name for it. On macOS each link
writes a fresh `LC_UUID` and an ad-hoc code signature. Swift's compilation
cache keyed macro plugins by their bytes and missed about 3,700 compiles
between two otherwise identical builds for that reason
([research §4.1](../research/compilers-2026.md#41-the-macro-plugin-cache-key-bug)).
tinymold therefore:

- keys a dependent on the **ActionKey of the action that built the
  proc-macro** (or build-script binary) whenever tinymold built or restored
  it, and falls back to `BLAKE3(dylib)` only for binaries of unknown origin;
- before trusting the identity, checks that the dylib on disk is the output
  that ActionKey's ActionResult records (digest compare via the stat cache),
  and treats a mismatch as a miss;
- passes `-C link-arg=-Wl,-no_uuid` when linking proc-macros and build
  scripts on Apple targets, so the bytes are stable as well. The flag is part
  of the producing action's arguments and therefore of its key.

## Two-phase lookup

The input file set and the env vars a crate reads are only known from the
dep-info of a previous compile. tinymold resolves this the way ccache's direct
mode and sccache do:

1. Compute the **pre-key** from `PreKeyInputs`, which needs only argv, the
   environment and the upstream artifacts.
2. Look up the pre-key. It maps to one or more recorded
   `(input path set, env name set)` pairs from earlier compiles.
3. For each recorded pair, hash those files (via the stat cache) and read
   those env values, compute the full **ActionKey**, and look it up. The
   first hit wins.
4. On a total miss, run rustc. Parse the new dep-info, record its input path
   and env name sets under the pre-key, compute the ActionKey from what was
   actually read, and store the ActionResult.

If a recorded input file no longer exists, that candidate is skipped, not
treated as an error. Step 3 never has to guess: an ActionKey is only ever
computed from a complete recorded set.

## Not in the key

| Excluded | Why |
|---|---|
| mtimes, inode numbers | Content is hashed instead; mtimes change on every checkout |
| The absolute workspace path, `CARGO_HOME` path, build-dir path | Replaced by `${WS}`, `${CARGO_HOME}`, `${OUT}` |
| `--out-dir`, `-C incremental=<dir>` | Where outputs go, not what they are |
| `--remap-path-prefix` flags injected by tinymold | A function of the roots, which are themselves normalized |
| `--error-format`, `--json`, `--color`, `--diagnostic-width` | Presentation only; diagnostics are stored in a canonical form and re-rendered (Phase 3 spec decides whether stored stderr is per format) |
| Jobserver fds, `CARGO_MAKEFLAGS`, `-Zthreads` thread counts | Scheduling, not output |
| Terminal and locale variables not listed in dep-info | Not read by the compiler for output |
| Lint-level flags (`-A`, `-W`, `-D`, `-F` and long forms) on a unit with `--cap-lints allow` | rustc ignores them there; outputs and diagnostics are identical. `--force-warn` stays ([test-cache spec](test-cache.md#flag-scoping)) |

`-C metadata` and `-C extra-filename` stay in the key: they change symbol
names and therefore output bytes.

## What breaks reuse, and tinymold's mitigation

| # | Breaks reuse | Effect | Mitigation |
|---|---|---|---|
| 1 | **Absolute paths** in debuginfo, `file!()`, `.rmeta` spans and dep-info | Two worktrees produce different bytes and keys | Normalize paths in the key; inject `--remap-path-prefix` per root; scan outputs for residual absolute paths and refuse to share them; an input outside every root makes the action uncacheable with an explained reason |
| 2 | **Incremental compilation** (dev default for workspace members) | Outputs depend on prior session state and are not reproducible | Cache them locally with `verified: false`; never share to the remote tier unless verification confirms them; keep incremental dirs in the per-key incremental-dir store so a switch does not start cold |
| 3 | **Crate types sccache refuses** (`bin`, `dylib`, `cdylib`, `proc-macro`) | Depend on the linker and system libraries | Add linker path, digest and version to the key, and use upstream `.rlib` digests for linking actions |
| 4 | **Proc macros reading undeclared files, env or time** | A hit can return a stale expansion | Key on the proc-macro's identity (its producing ActionKey) plus everything dep-info records; verification sampling catches impure macros; a denylist marks known-impure macros uncacheable |
| 5 | **Non-hermetic build scripts** (probe `cc`, pkg-config, `PATH`, time) | Outputs vary by host | Key dependents on the *outputs* of the build script (`OUT_DIR` digest and `cargo:` directives) rather than trusting it to be pure; cache the build script run itself only when its `rerun-if-*` declarations are complete |
| 6 | **`RUSTFLAGS` churn** (rust-analyzer, clippy, test and build with different flags) | Every unit re-keyed, including dependencies | `tinymold doctor` reports conflicting flag sets; the flag stabilizer moves global flags into `target.<triple>.rustflags` and profile overrides and gives rust-analyzer its own target dir. Lint flags leave the key of `--cap-lints allow` units, and `cargo llvm-cov` instruments only workspace crates, so dependencies keep one key across check, clippy, build, test and coverage ([test-cache spec](test-cache.md#flag-scoping)) |
| 7 | **Feature unification across invocations** (`-p a` vs `-p b`) | Two differently-featured builds of one dependency | Both variants are cached side by side, so alternation hits; a canonicalization report suggests a workspace-hack or `feature-unification = "workspace"` |
| 8 | **Mtime-based freshness** after checkout, restore or `git switch` | Cargo rebuilds content-identical units | The wrapper turns those rebuilds into hits; the checksum-freshness feature restores mtimes from a content manifest so cargo does not even schedule them |
| 9 | **Nondeterminism** (timestamps in build scripts, `HashMap` order in generated code) | Same key, different bytes; breaks early cutoff | Verification sampling detects it and marks the entry; early cutoff is only applied to crates whose rmeta has been observed to be stable |
| 10 | **Toolchain drift** (a new nightly, a different sysroot) | rlibs from another rustc fail SVH checks | `rustc_digest` and `sysroot_digest` are in every key, so drift is a miss, never a bad hit |
| 11 | **Same filename, different bytes** (41 of 209 rlibs across two worktrees, cause unknown) | Cargo's filename hash is not a content key | tinymold keys on full inputs and stores outputs by their own digest, never by filename ([hashing §4.2](../research/hashing-and-cache-server.md#42-dedup-potential-across-projects-and-worktrees-measured)) |

## Acceptance criteria (for the Phase 3 spec)

- A golden test pins the digest of a fixed `KeyInputs` value.
- Two checkouts of the same commit at different absolute paths produce the
  same ActionKey for every unit of the benchmark corpus that has no input
  outside a known root.
- Changing any single included field changes the key; changing any excluded
  field does not. Both are covered by table-driven tests.
- Every miss is attributed by `tinymold explain` to a named field.

## Open questions

- Final canonical encoding (deterministic CBOR or a hand-rolled
  length-prefixed format; the latter avoids a dependency).
- Whether stored diagnostics are rendered once per `--error-format` or stored
  as JSON and re-rendered.
- How many recorded input sets to keep per pre-key before evicting the oldest.
- Whether `ActionResult` manifests should carry enough of cargo's unit
  identity to translate to and from cargo's proposed `entries/<pkg>/<unit-hash>`
  cache ([ADR 0004](../adr/0004-store-layout-aligned-with-cargo.md)).
