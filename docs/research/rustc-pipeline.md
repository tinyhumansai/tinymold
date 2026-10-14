# rustc / Cargo Compile Pipeline and Acceleration Techniques (research for tinymold)

Compiled 2026-10-14. Status claims were checked against Rust project-goal pages, the Rust blog, and
the Inside Rust blog where possible. Items marked *(unverified)* come from memory or secondary
coverage and should be re-checked before tinymold depends on them.

---

## 0. Executive takeaways

1. **The worst pain is incremental rebuilds after small edits.** That was the top complaint in the
   2025 Rust Compiler Performance Survey (3,700+ responses, mean satisfaction 6/10). 55% of
   respondents wait more than 10 s for a rebuild of their worst project, and ~45% of people who left
   Rust cited compile times. Three things drive it: workspace dependents rebuild unnecessarily, links
   run from scratch every time, and derive expansion isn't cached.
   <https://blog.rust-lang.org/2025/09/10/rust-compiler-performance-survey-2025-results>
2. **Big structural fixes are still unstable or stalled upstream.** The parallel frontend is
   nightly-only, the Cranelift backend is nightly-only, proc-macro caching is behind
   `-Zcache-proc-macros`, the shared build cache is a 2026 goal, and "Relink, don't rebuild" (RDR) is
   marked "will not complete". That leaves room for an external tool that packages these
   capabilities safely for stable users.
3. **Config wins are large, cheap, and underused.** In the survey, 42% of respondents had tried no
   build-performance mechanism at all. Easy wins:
   - a fast linker;
   - lower debuginfo;
   - optimized dependencies;
   - fewer features;
   - a separate target dir for rust-analyzer.

---

## 1. Where the time goes

### 1.1 Pipeline stages

| Stage | What happens | Typical cost driver |
|---|---|---|
| Cargo resolution and fingerprinting | Builds the unit graph and checks mtime/hash fingerprints in `target/*/.fingerprint` | Large workspaces, mtime churn (docker, git checkout) |
| Build scripts (`build.rs`) | Compiled and run before the crate. They serialize the graph, and `-sys` crates often run `cc`/cmake | Native C builds (openssl-sys, ring, rocksdb), `rerun-if-changed` misuse |
| Lex/parse | `rustc_parse` produces the AST | Rarely dominant |
| Macro expansion and name resolution | `rustc_expand` and `rustc_resolve`. Proc macros run as dylibs inside rustc | serde/derive-heavy code. Proc-macro crates and their syn/quote deps sit on the critical path |
| HIR lowering, type collection, type check, trait solving | The query-driven frontend | Trait-heavy generic code (diesel, axum, bevy). The new trait solver is in progress |
| Borrowck (MIR build, NLL, Polonius later) | MIR construction and borrow checking | Huge functions, async state machines |
| MIR optimizations | Includes the MIR inliner (on by default for optimized builds since ~1.70) | Usually small |
| Monomorphization collection and CGU partitioning | Collects mono items and splits them into codegen units | Generic instantiation explosion. Measure with `cargo llvm-lines` and `-Zprint-mono-items` |
| LLVM IR gen and LLVM optimization/codegen | Runs in parallel per CGU | Dominates release builds and is significant in debug builds. Debuginfo inflates it |
| Metadata (`.rmeta`) emission | Lets dependents start (pipelining) | Spans and source maps make it change on trivial edits, which triggers downstream rebuilds |
| Linking | Runs `cc`, then ld/lld/mold/wild/ld64/ld-prime | Debuginfo size, static linking of every dependency. Repeated in full on every change |

Key facts:
- `cargo check` stops after metadata, with no codegen. It doesn't share artifacts with
  `cargo build`, so the frontend work is duplicated (noted in the 2025 survey results).
- Debug builds of leaf binaries are often **link-bound**. Large dependency graphs make clean builds
  **frontend- and LLVM-bound**.

### 1.2 The query system and incremental compilation
- rustc is organized as memoized **queries** (`type_of`, `typeck`, `optimized_mir`,
  `codegen_unit`...). Each query run records edges in a **dependency graph**.
- With `-Cincremental=<dir>` (the default for the Cargo `dev` profile), the dep graph and selected
  query results are serialized to `target/debug/incremental/`. On the next session rustc runs the
  **red/green algorithm**. Inputs are hashed (stable hashing, `HashStable`). A node is green if all
  its inputs are green, which it proves through `try_mark_green` without re-executing. Otherwise it
  re-executes, and if the result hash is unchanged it is green anyway (early cutoff).
- **Codegen reuse is per CGU.** If a CGU's inputs are green, its object file is reused
  ("post-LTO"/"pre-LTO" reuse states appear in `-Zincremental-info`). Incremental builds use 256
  CGUs by default, against 16 for non-incremental builds, to make reuse finer-grained.
- **Limits:**
  - Incremental state is **per crate per session**.
  - Parsing, expansion, and name resolution are not incremental. "Incremental Systems Rethought",
    a 2026 goal, aims to extend incrementality into those phases.
  - Any change to a crate's `.rmeta` dirties all dependents.
  - Linking is never incremental.
  - Compiler-dev guide: <https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation.html>

### 1.3 Measurement tools
- `cargo build --timings` (stable since 1.60) writes an HTML Gantt chart of units, showing the
  frontend/codegen split, the critical path, and concurrency.
  <https://doc.rust-lang.org/cargo/reference/timings.html>. Link times in the timings report are
  still unstable (2025 survey).
- `RUSTFLAGS=-Zself-profile` (nightly) writes `.mm_profdata`. View it with measureme's
  `summarize`, `crox` (Chrome trace), or `flamegraph`. Add `-Zself-profile-events=default,args`
  for per-item detail. <https://github.com/rust-lang/measureme>
- `-Ztime-passes` gives coarse pass timings. `-Zincremental-info` shows CGU reuse.
- `cargo llvm-lines` shows generic bloat. `cargo bloat --time` shows per-crate time.
  `-Zmacro-stats` (2025) shows how much code macro expansion produces.
- rustc-perf (<https://perf.rust-lang.org>) is the canonical benchmark suite and dashboard. It
  covers check/debug/opt × full/incr-full/incr-unchanged/incr-patched scenarios, and every rustc
  PR can be perf-run.
- Cargo `-Zbuild-analysis` (2025 goal) logs why each unit was rebuilt. *(unverified on exact flag
  name and status)*

---

## 2. Parallelism inside and across crates

### 2.1 Pipelined compilation (stable since 1.38, 2019)
- Cargo starts a dependent crate as soon as its dependency has emitted `.rmeta`, without waiting
  for codegen. Reported clean-build wins were roughly 10–30% on wide graphs.
  <https://internals.rust-lang.org/t/evaluating-pipelined-rustc-compilation/10199>
- Limits:
  - Binaries, proc macros, and build scripts need the full `rlib`/dylib, so they serialize.
  - The proc-macro → syn → quote chain is a well-known critical-path bottleneck.

### 2.2 Codegen units
- `codegen-units=N` splits LLVM work across threads. Trade-offs:
  - More CGUs means more parallelism but less inlining and some duplicated instances.
  - `codegen-units=1` plus LTO gives the best runtime and the slowest build.

### 2.3 Parallel frontend (`-Zthreads=N`)
- Nightly since Nov 2023. The Inside Rust post reported **up to 50% wall-time reduction** on the
  frontend with 8 threads for some crates. Gains are smaller under a parallel Cargo build, because
  the jobserver is shared.
  <https://blog.rust-lang.org/2023/11/09/parallel-rustc.html>
- Status:
  - **2025 H1:** occasional deadlocks, and reduced performance above 16 threads.
  - **2026 goal "Promoting Parallel Front End" (accepted):** deadlocks and ICEs have been fixed,
    the frontend is enabled in bootstrap, and an incremental-in-parallel race has been fixed. Still
    to come: a dedicated test suite, then a stabilization report.
  - **Not stable as of Oct 2026.**
  - <https://goals.rust-lang.org/2026/parallel-front-end.html>
  - <https://rust-lang.github.io/goals/2025h1/parallel-front-end.html>
- Practical use: `RUSTFLAGS="-Zthreads=8"` on nightly helps most for **single large crates**, such
  as the final binary crate in incremental dev loops.

### 2.4 `-Zshare-generics`, polymorphization, MIR inlining
- `share-generics` lets downstream crates reuse monomorphizations already instantiated in upstream
  crates instead of re-instantiating them. It is on by default for unoptimized builds (opt-level
  0/1), and you can force it with `-Zshare-generics=y`. Turning it on for release builds trades
  inlining for compile time. *(default behaviour unverified for latest rustc)*
- **Polymorphization** (`-Zpolymorphize`) is **removed**. Compiler-team MCP #810 (Nov 2024)
  deleted it and Rust 1.85 dropped the flag. It had no users and was buggy. A new "Polymorphic code
  generation experiment" appears in the 2026 roadmap (unfunded).
  <https://github.com/rust-lang/compiler-team/issues/810>
- **MIR inlining** is on at opt-level ≥ 1. It reduces the IR handed to LLVM and helps release
  compile times and runtime somewhat.

### 2.5 `-Zhint-mostly-unused` (2025)
- Defers codegen of a crate's items until a downstream crate actually uses them. It is meant for
  huge API crates (aws-sdk-*, windows, web-sys). Apply it per package only: a Cargo lint warns
  against a blanket use.
- Fuchsia measured one big dependency's release build going from **4m07s to 2m04s**, and its debug
  build from **2m26s to 1m28s**.
- <https://blog.rust-lang.org/inside-rust/2025/07/15/call-for-testing-hint-mostly-unused>
- <https://doc.rust-lang.org/unstable-book/compiler-flags/hint-mostly-unused.html>

---

## 3. Codegen backends
- **Cranelift (`rustc_codegen_cranelift`, cg_clif):**
  - Install with `rustup component add rustc-codegen-cranelift-preview --toolchain nightly`. Enable
    with `-Zcodegen-backend=cranelift`, or per profile with Cargo `-Zcodegen-backend`
    (`[profile.dev] codegen-backend = "cranelift"`).
  - Platforms: Linux and x86_64 macOS. Unwinding is off by default and doesn't work on Windows or
    macOS (June 2025 progress report).
  - Measured gains are modest:
    - the rustc benchmarks show between negligible and ~30% per crate, averaging ~10–15%;
    - Zed incremental builds on 16 cores were 5% faster than LLVM with `debuginfo=none`, and 12%
      faster with line-tables-only.
  - The 2026 goal "cg_clif performance" ($65k funded) targets incremental codegen. The goal page
    admits Cranelift "has not yet (fully) delivered" on its developer-experience promise.
  - The usual trick is to build **dependencies with LLVM and opt-level ≥ 1, and the workspace with
    Cranelift**.
  - <https://goals.rust-lang.org/2026/improve-cg_clif-performance.html>
  - <https://blog.rust-lang.org/inside-rust/2020/11/15/Using-rustc_codegen_cranelift.html>
- **TPDE backend (2026 goal, cost TBD):** the TPDE paper claims it compiles **10–20× faster than
  LLVM -O0** with similar code quality. It targets debug builds.
  <https://goals.rust-lang.org/2026/roadmap-fast-builds.html>
- **GCC backend:** aimed at target coverage, not speed.

---

## 4. Proc macros and build scripts
- **Cost model:**
  - Each proc-macro crate is compiled for the host as a dylib, together with syn/quote/proc-macro2,
    and loaded into rustc. Every invocation re-runs on every incremental rebuild, because there is
    no expansion caching yet. syn `full` features are heavy.
  - The proc-macro crates themselves are built without optimization by default. Setting
    `[profile.dev.build-override] opt-level = 3` makes expansion faster but the first build slower.
- **Derive expansion caching:**
  - rust-lang/rust PR #145354 ("Cache derive proc macro expansion with incremental query", a
    revival of #129102) is behind `-Zcache-proc-macros`, tracking issue #151364.
  - It gave ~10% on a no-op rebuild of octocrab.
  - Caveat: rustc doesn't check macro purity, so non-pure macros can produce stale expansions.
  - A downstream fork reported **-35% incremental time on polkadot-sdk** from expansion caching.
  - <https://doc.rust-lang.org/unstable-book/compiler-flags/cache-proc-macros.html>
  - <https://forum.polkadot.network/t/i-reduced-polkadot-sdk-incremental-compile-times-by-35/6900>
- **Precompiled proc macros:**
  - dtolnay's **watt** runs proc macros compiled to Wasm inside a tiny interpreter or JIT, so users
    skip compiling syn. <https://github.com/dtolnay/watt>
  - **The serde_derive binary controversy (Aug 2023):** serde_derive 1.0.172+ shipped a precompiled
    x86_64-linux binary. It removed the syn compile cost, roughly 10 s+ of the clean-build critical
    path. It was reverted in 1.0.184 after a backlash over reproducibility, auditability, and
    opt-out. The lesson: precompiled macros need **toolchain-level, verifiable** distribution, such
    as reproducible builds or a sandbox. <https://github.com/serde-rs/serde/issues/2538>
  - Internals threads on Wasm proc-macro sandboxing and caching:
    <https://internals.rust-lang.org/t/caching-of-proc-macro-expansions/21466>
- **Build scripts:**
  - Missing `cargo:rerun-if-changed` means the script re-runs when any file in the package changes.
  - `-sys` crates compiling C dominate many clean builds. Use the system library via pkg-config, or
    precompiled vendored artifacts.
  - The survey notes that stabilizing language features (cfg-version style detection, etc.) would
    remove many build scripts.
- **Feature unification:**
  - Cargo unifies features per build (`resolver = "2"` keeps build-dependency, dev-dependency, and
    target features separate).
  - So `cargo build -p a` and `cargo build -p b` can compile *different* variants of the same
    dependency. Each invalidates the other and rebuilds churn.
  - **cargo-hakari** generates a `workspace-hack` crate that pins the unified feature set, so every
    package sees identical dependency builds. It is used by nextest, Omicron, and others.
    <https://docs.rs/cargo-hakari>
  - Cargo is working on a native fix: `-Zfeature-unification=workspace` (2025). *(unverified status)*

---

## 5. Caching

| Tool | Granularity | Limits |
|---|---|---|
| **sccache** (Mozilla) | Wraps rustc and hashes the inputs: args, env, sources from dep-info, extern rlibs. Backends: local, S3, GCS, Redis, GHA | Can't cache **incremental** compiles (dev-profile workspace crates), bin/dylib/cdylib/proc-macro crate types, or anything linked. Absolute paths and env must match. Best for CI and for dependencies |
| **Cargo `build-dir`** | The `build.build-dir` config (or `CARGO_BUILD_BUILD_DIR`) moves intermediates out of `target-dir`. Introduced in Cargo 1.91 | Not shared. The layout rework is still being stabilized. <https://blog.rust-lang.org/inside-rust/2025/11/24/this-development-cycle-in-cargo-1.92/> |
| **Cargo cross-workspace cache** | 2026 accepted goal: a per-user shared cache of dependency builds, keyed on the unit hash, first available on nightly | Not shipped. <https://goals.rust-lang.org/2026/cargo-cross-workspace-cache.html> <https://goals.rust-lang.org/2025h2/cargo-build-dir-layout.html> |
| **Swatinem/rust-cache, cargo-chef** | CI: cache `target`/`~/.cargo`. Docker: a dependency layer | Coarse. Mtime fingerprints break after restore. `-Zchecksum-freshness` fixes that |
| **Bazel `rules_rust`** | Per-crate actions, hermetic, remote cache and remote execution | Needs BUILD files (crate_universe generates them). Has its own rust-analyzer integration. <https://bazelbuild.github.io/rules_rust/> |
| **Buck2 + reindeer** | Meta's Rust-native build system with remote execution. Reindeer generates targets from Cargo.toml | Heavy adoption cost. <https://buck2.build>, <https://github.com/facebookincubator/reindeer> |

- **`-Zchecksum-freshness`:** Cargo compares file size and then a content checksum emitted by rustc
  (`-Zchecksum-hash-algorithm=blake3`) instead of mtimes. This is essential for restored caches,
  `git switch`, and docker COPY.
  <https://doc.rust-lang.org/nightly/nightly-rustc/cargo/core/compiler/fingerprint/index.html>
- **`-Zbinary-dep-depinfo`:** rustc writes the rlibs and dylibs it loaded into the `.d` file, so a
  driving tool can track binary inputs. Bootstrap uses it, and it is useful for any external cache
  or daemon.
- **`-Zembed-metadata=no`** (2025): keeps metadata only in `.rmeta`, not duplicated in the rlib, so
  `target` shrinks.

---

## 5b. Cacheability: what rustc outputs can be reused, and what breaks reuse

**Reusable outputs per unit** (in `target/<profile>/deps`):
- `.rlib`: an ar archive of objects plus `lib.rmeta`, with the metadata embedded unless
  `-Zembed-metadata=no` is set.
- `.rmeta`: metadata only, used for check and pipelining.
- `.d`: dep-info, listing source files and `# env-dep:` entries for `env!` and `option_env!`.
- proc-macro `.so`/`.dylib`.
- build-script binaries, with their `out/` dir and `output` (the `cargo:` directives).
- **Non-reusable across machines:** the `incremental/` session dirs. They hold absolute paths and
  per-session dep graphs, and sccache refuses to cache any `-Cincremental` compile.

**The full cache key for one unit:**

| Component | Detail |
|---|---|
| Toolchain | The exact rustc commit, `rustc -vV` host and LLVM. An rlib from another rustc fails with "compiled by an incompatible version" (SVH/`StableCrateId` checks) |
| Target and codegen flags | `--target`, `-Ctarget-cpu`/`-feature`, opt-level, debuginfo, panic strategy, `codegen-units`, LTO, `-Cdebug-assertions`, `overflow-checks`, backend |
| Crate identity | Crate name, edition, crate-type, `--cfg feature=…`, and `-C metadata=<hash>`. Cargo derives that hash from the package id, source, enabled features, profile, target, rustc version, and the dependencies' metadata hashes. **Any feature or profile change renames the artifact (`libfoo-<hash>.rlib`) and invalidates every dependent** |
| Inputs | Every file in `.d`. Every `--extern` rlib/rmeta, including its content (its SVH flows into ours). `RUSTFLAGS`/`CARGO_ENCODED_RUSTFLAGS`, and **each tracked env var** (`env-dep`, `CARGO_PKG_*`, `CARGO_MANIFEST_DIR`, `OUT_DIR`) |
| Build-script outputs | `cargo:rustc-cfg`, `rustc-env`, `rustc-link-*`, and `OUT_DIR` contents (generated code). These need a cache of their own: the script's inputs are whatever `rerun-if-changed` and `rerun-if-env-changed` declare, and with none declared the whole package is the input |

**What breaks reuse in practice:**
1. **Absolute paths.** They are embedded in debuginfo (`DW_AT_comp_dir` and file names), in panic
   location strings (`file!()`), in `.rmeta` spans and source-file tables, and in dep-info. Two
   checkouts or worktrees at different paths produce different outputs and different keys.
   - Fixes: `--remap-path-prefix=<abs>=<rel>` (stable), Cargo's `trim-paths` profile option
     (RFC 3127, nightly `-Ztrim-paths`), and a stable `CARGO_HOME` location so registry sources
     (`~/.cargo/registry/src/index.crates.io-<hash>/`) share one path.
   - sccache needs identical cwd/paths or its base-dir normalization *(unverified option name)*.
2. **Incremental mode.** `CARGO_INCREMENTAL=1` (dev default) applies to workspace members. Their
   outputs aren't reproducible and can't be cached remotely. Cache them locally only, or build
   members with incremental off on CI.
3. **Non-cacheable crate types.** sccache skips `bin`, `dylib`, `cdylib`, and `proc-macro`, plus
   anything invoking the linker, because the output depends on system linker and library state. A
   tinymold cache can cache proc-macro dylibs if it keys on host triple, toolchain, and linker
   identity.
4. **Proc macros.** Their expansions can read files, env, or time without declaring it
   (`include_str!` is tracked, but `std::fs` reads inside a macro are not). Cache-key soundness
   relies on `.d` capturing every input. The `tracked_path`/`tracked_env` proc-macro APIs are still
   unstable.
5. **Build scripts.** They are non-hermetic: probing `cc`, pkg-config, system libraries, `$PATH`,
   and the time. Outputs vary by host. Treat them as cacheable only when their declared inputs are
   complete, or sandbox them.
6. **RUSTFLAGS churn.** Changing `RUSTFLAGS` (even reordering) re-keys *everything*, including
   dependencies. rust-analyzer, clippy, `cargo test`, and `cargo build` with different flags or
   features thrash a shared target dir. `[profile.*.package]` overrides and `target.<triple>.rustflags`
   are finer-grained than a global `RUSTFLAGS`.
7. **Feature unification across invocations.** `-p a` and `-p b` produce differently-featured
   dependencies, so there are two keys and two rlibs (§4, cargo-hakari).
8. **Mtime-based freshness.** After a cache restore, git checkout, or docker COPY, Cargo considers
   units dirty even when the content is identical. `-Zchecksum-freshness` fixes this.
9. **Nondeterminism.** Rust output is mostly reproducible given identical paths and flags. Known
   leaks are embedded build timestamps in some build scripts, `HashMap` iteration in
   generated code, and LLVM parallel CGU ordering (deterministic in practice).
   <https://reproducible-builds.org/docs/>

**What a cache can and can't skip:** a hit on a dependency rlib skips the frontend and codegen
entirely. A hit on the final binary is only possible if every input matches. For the edit loop,
the dominant remaining costs are the **leaf crate's incremental compile plus the full relink**,
which is why caching must be paired with link-time reduction.

---

## 6. Linking (link-time reduction)
- **Why linking dominates incremental loops.** Every edit relinks the whole binary from all
  dependency rlibs: every object plus its DWARF. Link time scales with total input bytes (symbols,
  relocations, debug sections), not with the size of the change. No mainstream ELF linker is
  incremental today.
- **Linux:** Rust **1.90 (Sep 2025)** made `rust-lld` the default for x86_64-unknown-linux-gnu.
  Opt out with `-Clinker-features=-lld`. The pre-release numbers showed ~**40% end-to-end
  reduction** in link-heavy cases and ~20% for debug builds.
  <https://blog.rust-lang.org/2025/09/01/rust-lld-on-1.90.0-stable>
- **mold** is usually 1.5–2× faster than lld on large binaries (`-C link-arg=-fuse-ld=mold`).
- **wild** linked rustc-driver in 486 ms, against 946 ms for mold and 1601 ms for lld. Zed measured
  incremental rebuilds at 3.9 s with wild, 5.0 s with mold, and 6.6 s with lld. wild is Linux-only,
  not yet incremental, and has a 2026+ integration goal paired with RDR.
  <https://github.com/davidlattimore/wild>
- **Shrink the linker input:**
  - `debug="line-tables-only"` or `0`, especially for dependencies;
  - `split-debuginfo=unpacked` (Linux `.dwo`, macOS default), which leaves DWARF out of the link;
  - on Linux, `-Wl,--compress-debug-sections=none`, since compression costs CPU *(unverified
    default)*;
  - fewer duplicate monomorphizations through `share-generics`.
- **Avoid relinking dependencies:**
  - **dylib dev builds.** Bevy's `dynamic_linking` turns bevy into one `dylib`, so relinks only
    touch the app objects and go from seconds to sub-second.
    <https://bevyengine.org/learn/quick-start/getting-started/setup/>
  - **Hot patching** (subsecond) goes further by linking only the changed functions against the
    running image's symbol table.
- **Test binaries.** Each `tests/*.rs` file is a separate binary, so N full links. Consolidating
  into one `tests/it/main.rs` cuts link count. nextest helps with execution, not linking.
- **macOS:**
  - Xcode 15+ **ld-prime** is about as fast as sold or mold.
  - `split-debuginfo=unpacked` avoids `dsymutil`.
  - **XProtect** scans each new executable on first run, which costs about 1–2 s per build-script or
    test binary. Fix it by adding the terminal and IDE under *Privacy & Security → Developer Tools*.
    <https://oxc.rs/docs/contribute/development.html>
  - Exclude `target/` from Spotlight.
- **Windows:** `-Clinker=rust-lld`, and a Dev Drive for `target`.

---

## 7. Hot reload and IDE model (brief)
- **subsecond (Dioxus 0.7, `dx serve --hotpatch`):** functions are called through a jump table.
  On a save it recompiles the tip crate only, thin-links a patch dylib against the running binary's
  addresses, and swaps the table pointers. Limits: tip crate only, no layout changes, statics not
  re-run. <https://docs.rs/subsecond>, <https://dioxuslabs.com/blog/release-070>
- **rust-analyzer (salsa)** is fine-grained, in-memory, and never does codegen. 35% of survey
  respondents hit RA/Cargo target-lock contention. The fix is `rust-analyzer.cargo.targetDir = true`.
- **cargo-watch, bacon, watchexec** are trigger loops only.

---

## 8. Practical configuration (what tinymold could emit)

```toml
# Cargo.toml
[profile.dev]
debug = "line-tables-only"        # or 0; survey: 2–30% cycle win from line-tables-only
split-debuginfo = "unpacked"      # Linux: skip copying DWARF into the binary
incremental = true                 # default

[profile.dev.package."*"]
opt-level = 1                      # or 2/3: fast runtime deps, built once and cached
debug = false                      # deps rarely need debuginfo

[profile.dev.build-override]
opt-level = 3                      # faster proc macros and build scripts at execution time

[profile.release]
codegen-units = 16                 # vs 1 for max runtime perf
lto = "thin"                       # "off" for faster local release builds
```

```toml
# .cargo/config.toml (nightly extras guarded)
[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=mold"]      # or wild; lld is default since 1.90
[unstable]
codegen-backend = true
checksum-freshness = true
[profile.dev]
codegen-backend = "cranelift"
[build]
rustflags = ["-Zthreads=8", "-Zshare-generics=y"]
```

Process and structural tips (corrode.dev "Slow Rust Builds?", ordered roughly by impact):
<https://corrode.dev/blog/tips-for-faster-rust-compile-times/>
- **Update the toolchain.** Typical yearly gains are double-digit percentages. nnethercote's
  regular "How to speed up the Rust compiler" posts reported -4.6% mean over two months in
  mid-2026. <https://nnethercote.github.io/>
- **Use `cargo check` and clippy for the inner loop**, and `cargo nextest` for tests: tests run as
  separate processes and finish faster.
- **Prune the dependency graph:**
  - `cargo machete`, a fast heuristic, and `cargo udeps` (nightly, exact) find unused dependencies;
  - `cargo tree -d` finds duplicate versions, and `cargo tree -e features` shows the features in
    play;
  - `default-features = false` turns off what you don't need.
- **Split crates for parallelism.** Break a giant crate into a DAG with wide fan-out. Keep heavy
  generic code out of the leaf crate that changes most often, so that the frequently edited crate
  is small.
- **Choose dyn over generics on boundaries:**
  - write a `fn f<P: AsRef<Path>>(p: P) { inner(p.as_ref()) }` outer shim around a non-generic
    inner function (the momo pattern);
  - use `&dyn Trait` in cold paths;
  - avoid `impl Trait`-heavy builder chains;
  - measure with `cargo llvm-lines`.
- **Use fewer, lighter proc macros.** Use `serde` without `derive` where you can. Consider
  `miniserde` or `nanoserde`, and avoid `syn` `full`.
- **Linux tmpfs or a fast NVMe for `target`.** Caveat: tmpfs memory pressure. Prefer `build-dir`
  on fast local disk.
- **CI:** use sccache for dependencies, rust-cache, `CARGO_INCREMENTAL=0` (incremental state is
  useless there), and `debug=0`.

---

## 9. Newer ideas and in-flight proposals

| Idea | Status (Oct 2026) | Notes |
|---|---|---|
| **Relink, Don't Rebuild (RDR)** (rust-lang/goals#400) | 2025H2 goal, marked "**will not complete**" on the recent-updates pages. Still listed on the 2026 Fast Builds roadmap claiming **5–10×** for common edits | Problem: comment, format, `dbg!`, or private-body edits change `.rmeta` (spans, source map), which rebuilds every dependent. PoC PR #143249 hashed `.rmeta` minus spans, which hurt diagnostics. <https://goals.rust-lang.org/2025h2/relink-dont-rebuild.html> <https://github.com/rust-lang/rust/pull/143249> |
| **Incremental Systems Rethought** | 2026 goal | Share incremental bases across invocations with different flags (check vs build). Make expansion and resolution incremental |
| **MIR-only rlibs** | Experimental (saethlin, `-Zbuild-std`-adjacent) *(unverified flag name)* | Dependencies ship only MIR. All codegen happens once at the final crate, with global dedup of monomorphizations. Clean builds were mixed, and incremental behaviour is the open question |
| **Crate slicing** | Future roadmap | Compile only the used parts of dependencies. Experiments showed **1.4–30×**. Requires re-architecture |
| **Better Cargo↔rustc parallelization** | Future | A jobserver-aware split of frontend and codegen threads |
| **check/build cache sharing** | Tracked Cargo issue | Today `check` and `build` duplicate the frontend |
| **Wild incremental linking** | 2026+ | Prerequisite for RDR-style fast relinks |
| **Lower default dev debuginfo, built-in debugging profile** | Under consideration in Cargo | Survey follow-up |
| **`target` GC (`cargo clean gc`)** | Nightly `-Zgc` | Disk, not speed |
| **Dylib dev builds** (Bevy-style, `-Cprefer-dynamic`, `crate-type=dylib` for a deps bundle) | Works on stable | Big relink wins. Problems: std dylib duplication and platform rpath quirks |

---

## 10. Ideas tinymold could adopt or build (impact × difficulty)

The ideas are weighted toward tinymold's pillars: **build caching, linking, and a hot cache
server**. Hashing algorithms and daemon internals are covered by a separate research track.

Impact: H = 2× or better on the targeted workflow, M = 15–50%, L = under 15%.
Difficulty: E = days, M = weeks, H = months or a compiler fork.

| # | Idea | What it does | Impact | Difficulty |
|---|---|---|---|---|
| 1 | **Path-normalizing rustc wrapper** | Injects `--remap-path-prefix` for the workspace root, `CARGO_HOME` and `build-dir`, and canonicalizes `.d`, `env-dep`, and `CARGO_MANIFEST_DIR` in keys, so N worktrees and checkouts on one box (and across CI and laptops) share artifacts (§5b.1) | H for worktree-heavy and cold-checkout workflows | M |
| 2 | **Dependency artifact cache (local plus hot server)** | Content-addressed cache of rlib/rmeta/`.d` for non-workspace units, keyed per the §5b table (toolchain, target flags, `-C metadata`, inputs, extern SVHs). Fills the gap until Cargo's cross-workspace cache ships | H for clean, CI, and new-worktree builds | M |
| 3 | **Cache proc-macro dylibs and build-script outputs** | Things sccache refuses. Key proc macros on host triple, toolchain, and linker identity. Key build scripts on declared `rerun-if-*` inputs plus probed env and tools. Fall back to "uncacheable" when a script declares nothing | H for clean builds (syn, `-sys` C builds off the critical path) | M–H |
| 4 | **Cache-hit-rate auditor** | Explains every miss: RUSTFLAGS diff, feature-set diff, path leak, nondeterministic build script, mtime-only dirtiness. The prerequisite for trusting any cache | M (multiplies 2–3) | E–M |
| 5 | **Flag and feature stabilizer** | Moves global `RUSTFLAGS` into `target.<triple>.rustflags` and profile package overrides, generates a workspace-hack (hakari) or `feature-unification=workspace`, and isolates the rust-analyzer target dir. This stops cache thrash between check, test, build, and clippy | M–H | E–M |
| 6 | **Checksum freshness on stable** | Restores mtimes from a content manifest after a cache restore or git switch, so Cargo doesn't rebuild identical units (a stable-toolchain emulation of `-Zchecksum-freshness`) | M–H after restores and branch switches | E–M |
| 7 | **Linker autoselect and tuning** | wild, then mold, then lld (Linux) or ld-prime (macOS), wired per target. Also `split-debuginfo=unpacked`, `debug="line-tables-only"`, and deps with `debug=0` to shrink link input. Benchmarked on the user's binary | M–H (20–40% of incremental loop) | E |
| 8 | **Dylib dev mode generator** | Bundles heavy dependencies into one `dylib` crate (the Bevy `dynamic_linking` pattern), so incremental relinks touch only app objects. Handles the rpath fix and the duplicate-std check | H for link-bound loops | M |
| 9 | **Test-binary consolidation advisor** | Detects N integration-test binaries, each a full link, and proposes or codemods a single `tests/it` binary | M–H for test loops | E |
| 10 | **Cached, prelinked dependency archive** | Pre-links all dependency objects into one relocatable `.o` (`ld -r`) or a dylib stored in the cache, so the final link merges 2 inputs instead of thousands. A step toward incremental linking before wild ships it | M–H link time | M–H |
| 11 | **RDR-lite in the wrapper** | Hashes a span-insensitive normalization of a rebuilt crate's `.rmeta` (public items, signatures, inlinable and generic MIR). If it is unchanged, serve dependents from cache and only relink | H (roadmap claims 5–10× on body-only edits) | H (needs rmeta decoding or a patched rustc) |
| 12 | **Proc-macro expansion cache with purity allowlist** | Allowlisted pure derives (serde, thiserror...) with `-Zcache-proc-macros` on nightly, or wrapper-level memoization. Includes a periodic re-expand-and-diff verification | M (10–35% incremental on derive-heavy crates) | M–H |
| 13 | **Verified prebuilt host artifacts** | Reproducibly built, signed proc-macro and `-sys` artifacts served from the hot cache server: opt-in and auditable, avoiding the serde_derive mistake | H for clean builds | M |
| 14 | **Profile and config generator** | The §8 profile as a reversible patch, with before and after benchmarks | M–H | E |
| 15 | **macOS hygiene fixer** | XProtect Developer Tools, Spotlight exclusion, and ld-prime check | M on macOS test loops | E |
| 16 | **`tinymold doctor`** | `--timings` and self-profile classification: link-, frontend-, LLVM-, macro-, or build-script-bound | M (enabler) | E–M |
| 17 | **Nightly turbo mode (opt-in)** | `-Zthreads`, Cranelift for workspace crates, `-Zshare-generics`, `-Zhint-mostly-unused` per huge dependency, `-Zembed-metadata=no` (smaller cache objects) | M–H | M |
| 18 | **Benchmark harness** | rustc-perf-style scenarios on the user's repo: clean, incr-unchanged, comment, body, and signature edits, plus hit-rate and link time | Enabler | E–M |

Suggested sequencing:
1. Ship items 16, 18, 14, 7 and 15 as stable quick wins.
2. Build the cache core: items 1, 2, 4, 5 and 6, which is the differentiator.
3. Extend the cache to items 3 and 13 and the link work in 8, 9 and 10.
4. Research bets: items 11 and 12.

---

## Sources (primary; more inline above)
- Survey 2025: <https://blog.rust-lang.org/2025/09/10/rust-compiler-performance-survey-2025-results>
- Fast Builds roadmap 2026: <https://goals.rust-lang.org/2026/roadmap-fast-builds.html>
- Parallel frontend: <https://blog.rust-lang.org/2023/11/09/parallel-rustc.html>
- Cranelift backend: <https://goals.rust-lang.org/2026/improve-cg_clif-performance.html>
- RDR: <https://goals.rust-lang.org/2025h2/relink-dont-rebuild.html>
- Cargo build-dir and cross-workspace cache: <https://blog.rust-lang.org/inside-rust/2025/11/24/this-development-cycle-in-cargo-1.92/>, <https://goals.rust-lang.org/2026/cargo-cross-workspace-cache.html>
- Linkers: <https://blog.rust-lang.org/2025/09/01/rust-lld-on-1.90.0-stable>, <https://github.com/davidlattimore/wild>
- Incremental internals: <https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation.html>; perf: <https://perf.rust-lang.org>, <https://nnethercote.github.io/>
