# Plan: tinymold-link, a stateful fork of mold

- **Spec:** [`../specs/linker.md`](../specs/linker.md)
- **Decision:** [ADR 0009](../adr/0009-fork-mold-into-a-stateful-linker.md)
- **Research:** [`../research/mold-and-bun.md`](../research/mold-and-bun.md)
- **Roadmap:** L0-L2 sit beside Phase 4 (test builds and links), L5 replaces
  Phase 5's prelink-and-append spike, L3-L6 replace Phase 7's "incremental
  linking contributed upstream to wild".
- **Status:** L0 in progress.

Each phase is one or more pull requests, written test first: a failing test
for the behaviour, then the smallest change that passes it. Every pull request
ends with the four contract commands green. A phase starts only after the
previous one's exit criteria are met, and L1 can stop the plan.

## Facts checked before writing the plan

Read at mold [`c522789`](https://github.com/rui314/mold/tree/c522789) and Bun
[`f841e367`](https://github.com/oven-sh/bun/tree/f841e367):

- mold 3.0 is a cargo workspace of Rust crates, MIT-licensed, with the ELF
  linker generic over the target and instantiated once per target crate.
- `new_object_file` in `elf/reader.rs` is the single place an object file is
  parsed and its globals registered; archive members reach it through
  `read_input_files`.
- mold has no state that survives a link; `--perf` and `--stats` already give
  a per-pass split and counters.
- mold's Mach-O backend is reached by invoking the binary as `ld64*`.
- Bun's transpiler cache names entries by content hash and checks an options
  hash and a version inside the entry; its manifest cache reads typed arrays
  by casting after validation.

## L0: import the fork

Goal: `tinymold-link` builds in the workspace and links like mold.

1. Copy mold `c522789`'s `common/`, `elf/`, `macho/`, `cli/` and the
   `elf-x86_64`, `elf-arm64`, `macho-arm64` and `macho-x86_64` arch crates into
   `crates/tinymold-link-*` as named in ADR 0009, keeping file contents
   verbatim.
2. Rename packages only. Internal dependencies use
   `mold-common = { package = "tinymold-link-common", ... }` so source paths
   such as `mold_common::` do not change. Add the internal entries to the root
   `[workspace.dependencies]` with `version = "=<workspace version>"`.
3. Cut `cli/` down to the four kept targets: features and dispatch for the
   other 18 are removed, and the change is listed in `FORK.md`.
4. Keep mold's `LICENSE` in each forked crate, `license = "MIT"`, and copy
   mold's `rustfmt.toml` into each crate so `cargo fmt --all -- --check` keeps
   upstream formatting.
5. In a separate commit, give the forked crates their own lint table allowing
   `unsafe_code`, instead of `[lints] workspace = true`.
6. Add `crates/tinymold-link/FORK.md`: upstream repository and commit, the
   list of local changes, and how to merge a newer upstream commit.
7. Bring mold's shell-test runner over as `tinymold-link-tests`
   (`publish = false`), behind a feature so `cargo test --all-features` runs a
   smoke subset and the full suite runs on demand.

Exit criteria:

- the four contract commands pass with the fork in the workspace;
- `tinymold-link` links ripgrep and bat on darwin-arm64 through
  `ld64.tinymold`, and the binaries pass their own tests;
- the smoke subset of mold's tests passes on macOS (Mach-O) and in Linux CI
  (ELF);
- the PR records how much the fork adds to a clean workspace build.

## L1: measure where a Rust link spends its time

Goal: decide with numbers whether the cache phases are worth building.

1. `bench/bench.py`: a `link` mode that runs `tinymold-link` with `--perf`
   and `--stats` on ripgrep and bat, cold and after a one-line edit to the top
   crate, on darwin-arm64 and Linux. Pin filesystem, output reuse, fork mode
   and thread count, as for every linker benchmark.
2. Record per pass: time in `read_input_files`, `resolve_symbols`,
   `create_merged_sections`, relocation scanning, copy, and build-id; and the
   share of parsed objects whose bytes did not change since the previous link.
3. Commit the results JSON with `bench/report.py` output.

Exit criteria: the result files exist for both platforms. If parsing and
merging of unchanged inputs is under 25% of the edit-loop link on both
platforms, stop after L1, write an ADR superseding the cache phases, and keep
the fork only for link-skip, the governor and Mach-O linking.

## L2: the per-input parse cache

Goal: unchanged objects are not parsed again. ELF first, then Mach-O.

1. **Format.** In a new module of the fork (`tinymold-link-common/cache/`),
   write the entry encoder and decoder from the spec: header, column
   directory, columns, string table built by a counting pass. Tests first:
   - a known-vector test pinning the exact bytes of a small entry;
   - round trip for a synthetic symbol table;
   - rejects: wrong magic, wrong version, wrong options hash, wrong content
     key, truncated entry, column out of bounds, misaligned column, string
     offset out of bounds, out-of-range enum value.
2. **Options hash.** List the parse-affecting arguments next to the code that
   reads them; a test fails if a new argument read by `ObjectFile::new` or
   `register_global_symbols` is missing from the list.
3. **Store.** Index files under `link/parse/` and entries via
   `tinymold-cas`'s `Store::insert_bytes`. Tests in `tempfile` stores: hit
   after store, miss on an empty store, reject removes the index file, store
   errors do not fail the link.
4. **ELF seam.** In `new_object_file`, under `TINYMOLD_LINK_CACHE`, look up
   before parsing and store after. Add the `--stats` counters.
5. **Differential test.** A fixture crate linked three ways (cache off, cold
   cache, warm cache) must give byte-identical outputs. Run it in CI on
   Linux.
6. **Mach-O seam.** The same for the Mach-O object parser, with the same
   differential test on macOS.
7. **Bench.** Re-run L1's benchmark with a warm cache.

Exit criteria: all tests above pass; byte-identical outputs on both
backends; the bench shows the warm-cache edit-loop link before and after on a
named machine. The PR claims only what those numbers show.

## L3: the merge-fragment cache

Goal: mergeable sections (`.debug_str`, `.rodata.str*`) of unchanged inputs
are not split and hashed again.

- Extend the entry with a fragment column: offsets, lengths and hashes for
  each mergeable section, produced by `convert_mergeable_sections`.
- Tests: known vector, differential output test on a debug build where
  `.debug_str` dominates.

Exit criteria: byte-identical outputs; measured reduction in
`create_merged_sections` time.

## L4: the warm daemon

Goal: dependency state stays in memory between links; `cargo test` links
share one copy of it.

Prerequisites, each its own PR with tests, and each offered upstream:

- per-link arenas instead of leaked `'static` inputs;
- errors as `Result` instead of `_exit`;
- a rayon pool owned by the link rather than built globally;
- relocation rewrites in a side table instead of inside the input mapping.

Then: `tinymold-link` connects to `tinymold-server` over its Unix socket and
falls back to an in-process link when the server is absent; the server keeps
parsed inputs keyed by content digest; an N-slot governor replaces
`MOLD_JOBS`.

Exit criteria: killing the server mid-link still produces a correct link; a
warm link of ripgrep after an edit is measured against L2; peak memory of
`cargo test --no-run` on bat is measured and bounded.

## L5: prelinked dependencies and append linking

Goal: an edit-loop link that only lays out and relocates the top crate.

- First link: lay out all dependency inputs and leave slack after each output
  section; record the layout in the store.
- Later links: if only top-crate objects changed and their symbols fit, write
  them into the slack and patch references, growing segments the way Bun's
  `exe_format` grows `.bun` on ELF and `__BUN` on Mach-O; on Mach-O shift
  `__LINKEDIT` and sign again.
- Anything else (symbol-set change, overflow, argument change) is a full link.

Exit criteria: append links pass the differential *behaviour* test (outputs
run the same tests; byte-identical output is not expected in this mode, which
is dev-only and opt-in); measured link time against L4.

## L6: incremental build-id and output

Goal: do not rehash or rewrite unchanged ranges of large outputs.

- Hash the output in stable shards and keep shard hashes from the previous
  link; recompute the build-id from changed shards only.
- Write only changed ranges into the reused output file.

Exit criteria: identical build-id to a full recomputation; measured time
saved on a debug output of at least 500 MB.

## Completion checklist

- [ ] L0: fork imported, contract commands green, FORK.md written
- [ ] L1: link benchmark with per-pass split on darwin-arm64 and Linux
- [ ] L1: go/no-go recorded
- [ ] L2: entry format and known vector
- [ ] L2: ELF seam and differential test
- [ ] L2: Mach-O seam and differential test
- [ ] L2: warm-cache benchmark
- [ ] L3: merge-fragment cache
- [ ] L4: prerequisites upstreamed or carried in FORK.md
- [ ] L4: daemon mode
- [ ] L5: append linking
- [ ] L6: incremental build-id and output
