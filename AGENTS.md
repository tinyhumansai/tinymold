# Repository Guidelines

This file is the single source of truth for how humans and coding agents work
in this repository. `CLAUDE.md` is a symlink to this file, so every agent reads
the same instructions.

tinymold is a fast content-addressed build cache: one server and one store,
with protocol front-ends for each ecosystem's remote cache (REAPI, Bazel
HTTP, Turborepo, Nx, Gradle) and client adapters for toolchains that need
one (the Rust wrapper, Go's `GOCACHEPROG`). Linking is out of scope
([ADR 0009](docs/adr/0009-a-cache-server-with-protocol-front-ends.md)). Read
[`README.md`](README.md) for the pitch,
[`docs/research/README.md`](docs/research/README.md) for why,
[`docs/specs/cache-server.md`](docs/specs/cache-server.md) for the server and
[`docs/specs/architecture.md`](docs/specs/architecture.md) for the Rust
adapter before changing behavior.

## Project Structure

This is a Rust 2024 cargo workspace rooted at a virtual `Cargo.toml`. Every
crate lives under `crates/`, one directory per package, each directory named for
the package it holds. There is no root package: the `tinymold` binary is
`crates/tinymold`, the same as any other member.

```text
Cargo.toml              # virtual workspace: members, [workspace.package],
                        # [workspace.dependencies], [workspace.lints]
crates/
├── tinymold-hash/      # BLAKE3-256 `Digest`, file hashing, field hasher
├── tinymold-cas/       # local content-addressed store + materialization
├── tinymold-key/       # rustc argument parsing, path roots, dep-info, keys
├── tinymold-wrap/      # the RUSTC_WRAPPER: lookup, restore, run and store
├── tinymold-remote/    # the remote tier: a REAPI v2 client (bazel-remote)
└── tinymold/           # the `tinymold` binary and its library half
    ├── src/
    │   ├── lib.rs      # crate docs + the entire public surface
    │   ├── main.rs     # argument parsing and exit codes only
    │   ├── error/mod.rs      # crate-wide `Error` and `Result<T>`
    │   └── <feature>/        # one directory per feature area
    │       ├── mod.rs        # module docs, wiring, smallest useful public API
    │       ├── types.rs      # substantial type definitions
    │       └── test.rs       # module-local unit tests
    ├── tests/          # integration tests against the public API only
    └── examples/       # runnable, compiled-in-CI usage examples
action.yml, save/       # the GitHub Action (setup and save), scripts in action/
deploy/                 # how to run the remote cache server (bazel-remote)
bench/                  # benchmark harness, corpus, results, and results page
docs/
├── research/           # the research behind the design, with sources
├── specs/              # behavior and architecture specifications
├── plans/              # roadmap and test-first implementation plans
└── adr/                # immutable architecture decision records
```

### Crate boundaries

Dependencies point one way: `tinymold-hash` ← `tinymold-cas` ←
`tinymold-wrap` ← `tinymold-remote` ← `tinymold`, with `tinymold-key` (pure
key computation, depending only on `tinymold-hash`) used by `tinymold-wrap`.
`tinymold-remote` is optional in `tinymold`, behind its default `remote`
feature. The planned `tinymold-store` (namespaces, entries, digest index,
GC; no network) sits on `tinymold-cas`, and `tinymold-server` (transport,
auth, protocol front-ends) sits on it, as described in the
[cache-server spec](docs/specs/cache-server.md).

- `tinymold-hash` answers "what is the name of these bytes". No storage, no
  cache policy.
- `tinymold-cas` answers "bytes in, bytes out, by name". No cache keys, no
  eviction, no network. Objects are immutable once committed.
- `tinymold-key` answers "which inputs decide this compile's output". Pure
  functions over arguments, paths and digests; no I/O, no processes.
- `tinymold-wrap` answers "serve this rustc call": hashing files, running
  rustc, and moving outputs in and out of the store. It reaches a remote
  only through its synchronous `Remote` trait.
- `tinymold-remote` answers "what does the shared cache hold": REAPI calls,
  SHA-256 naming, and batch upload. It is the only crate with an async
  runtime.
- `tinymold-store` answers "what is stored under this key, in this
  namespace". Shared by every protocol; no network, no async runtime.
- `tinymold-server` answers "serve this protocol's request": parsing each
  wire protocol onto the store. It never computes a client's keys.
- `tinymold` holds the command line and whatever composes the layers.

Put new code in the lowest crate that can hold it without pulling in anything
that crate deliberately avoids. Each crate's `src/lib.rs` says what it does
*not* hold; respect that rather than widening a crate for convenience.

Add a crate by creating `crates/<name>/` — `members = ["crates/*"]` picks it up
by existing. Inherit `version`, `edition`, `rust-version`, `license`, and
`repository` from `[workspace.package]`, take shared dependencies from
`[workspace.dependencies]`, add an internal entry there with
`version = "=<workspace version>"` (crates.io needs it), and opt into the
shared lint set with:

```toml
[lints]
workspace = true
```

Each feature area belongs in a focused module directory under a crate's `src/`.
A module root explains the module, wires its pieces together, and exposes the
smallest useful API. Move substantial type definitions into `types.rs` and put
module-local unit tests in a dedicated `test.rs`, wired from the bottom of the
module root with:

```rust
#[cfg(test)]
mod test;
```

Do not accumulate inline `mod tests` blocks in implementation files, and do not
let a general-purpose `utils.rs` or `helpers.rs` grow — those are a symptom of a
missing module. Prefer many small modules that each do one thing well over few
broad ones.

Keep public exports centralized in each crate's `src/lib.rs` so downstream users
have one predictable surface. Put each crate's error variants in its
`src/error/mod.rs` and return that crate's `Result<T>` from fallible public
APIs.

## Build And Test

Run every command from the repository root. These four are the contract; CI
runs exactly them, so a green local run should mean a green CI run.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --all-targets --all-features
cargo test --all-features
```

Supporting commands:

- `cargo fmt --all` — format before committing.
- `cargo test <filter>` — run a focused subset while iterating.
- `cargo test -p tinymold-cas` — run one crate's suite.
- `cargo run -p tinymold --example basic` — run the bundled example.
- `cargo run -p tinymold -- doctor` — check this machine's linkers and store.
- `cargo package --workspace --locked` — verify every crate would publish.
- `cargo doc --no-deps --all-features` — build the rustdoc CI also builds with
  `RUSTDOCFLAGS="-D warnings"`.
- `cargo test --doc` — run doctests alone when editing documentation examples.

Never skip, ignore, or delete a failing test to make a command pass. Fix the
root cause, or stop and report the blocker.

## Coding Style

Use standard `rustfmt` output and Rust 2024 idioms. Do not hand-format around
`rustfmt`, and do not add `#[rustfmt::skip]` without a comment explaining why.

- `snake_case` for modules, files, functions, methods, fields, and locals.
- `PascalCase` for types, traits, and enum variants; `SCREAMING_SNAKE_CASE` for
  constants and statics.
- Name things for what they are, not for their layer: `RetryPolicy`, not
  `RetryHelper`.
- Prefer small, typed APIs over stringly-typed ones. Accept `&str` and generic
  `impl Into<String>` at boundaries; return owned, concrete types.
- Keep the public surface minimal: default to private, and export deliberately
  from `src/lib.rs`.
- `unsafe` is forbidden workspace-wide by `[workspace.lints]` in the root
  `Cargo.toml`. If a project genuinely needs it, relax the lint in its own
  commit and document every invariant with a `// SAFETY:` comment.

### Errors

- One crate-wide `Error` enum per crate, in `src/error/mod.rs`, built with
  `thiserror`.
- Fallible public functions return `Result<T>`, the crate alias.
- Add a specific variant instead of stuffing context into a string; error
  messages are lowercase, without trailing punctuation.
- Do not `unwrap()`, `expect()`, or `panic!` in library code paths. They are
  fine in tests, examples, and genuinely unreachable states — where `expect`
  must carry a message explaining the invariant.
- Document a `# Errors` section on every public fallible function and a
  `# Panics` section on anything that can panic.

### Dependencies

Adding a dependency is a design decision. Before adding one, check whether the
standard library or an existing dependency already covers the need. When you do
add one:

- pin a caret range (`serde = "1"`), not an exact version;
- enable only the features you need, with `default-features = false` when that
  meaningfully trims the tree;
- gate anything optional behind a Cargo feature, documented in `Cargo.toml`;
- declare it once in the root `[workspace.dependencies]` when more than one
  crate needs it, and take it with `{ workspace = true }`;
- keep `tinymold-hash` and `tinymold-cas` free of async runtimes, network
  clients, and native libraries; those belong in the crates that need them;
- check whether a new dependency costs build time worth its keep: tinymold is a
  build-speed project and its own dependency graph is a fair test of that;
- leave a comment above the entry explaining *why* the crate is needed and what
  uses it — see the existing entries for the expected tone;
- prefer well-maintained crates with a compatible license.

Keep `Cargo.lock` committed; this workspace ships a single lockfile so CI and
releases are reproducible.

## Testing

- Module-local unit tests live in `crates/<crate>/src/<feature>/test.rs` and may
  touch private items.
- Integration tests live in `crates/<crate>/tests/` and exercise only the public
  API — they are the regression suite for the crate's contract.
- Anything persisted or shared — digests, store layout, cache keys, manifest
  formats — pins its exact encoding in a unit test, ideally against a known
  vector (see the BLAKE3 empty-input test). A silent encoding change poisons
  every cache built before it.
- Tests that touch the filesystem use `tempfile` directories, never the user's
  real store, and must pass whether or not the filesystem supports reflinks.
- Use descriptive, behavioral test names: `rejects_a_tampered_object`, not
  `test_store_2`.
- Cover the failure paths, not just the happy path. Every new error variant
  needs a test that produces it.
- For async behavior, standardize on one runtime (`tokio` as a dev-dependency
  for tests) rather than mixing runtimes.
- Tests must be deterministic and independent of network, wall-clock time, and
  execution order. Gate any live/network test behind a feature or an env var and
  name it `live_*` so it is easy to exclude.
- Maintain at least 90% line coverage in every source file. Add or update tests
  with every behavior change, and note any deliberately untested edge case in
  the pull request description.

Write the test first when fixing a bug: a failing test that reproduces the
report, then the fix that turns it green.

## Documentation

Write documentation for the reader who has never seen the code.

- Every public item gets a rustdoc comment. `missing_docs` is a warning that CI
  treats as an error.
- Start every `mod.rs` and `test.rs` with a concise module-level `//!`
  description.
- Each crate's `src/lib.rs` carries its crate-level overview: what the crate
  does, the primary entry points, and a short runnable example. It should also
  say what the crate deliberately does *not* hold, and why.
- Prefer concrete examples over vague description. Doc examples are compiled and
  run by `cargo test`, so they cannot drift.
- Complex modules must include a module-level `README.md` covering their design,
  public surface, and important operational constraints.
- Keep `README.md`, `docs/`, and module docs aligned with code changes in the
  same commit that changes behavior.
- Write accepted behavior and constraints in `docs/specs/` before creating a
  linked, implementation-ordered plan in `docs/plans/`. Specs define what and
  why; plans define how and in what sequence.
- Keep every Markdown file, including this one, at 500 lines or fewer. When a
  topic outgrows that, split it into focused files and link them from the
  nearest `README.md`.

## Benchmarks And Performance Claims

tinymold's value is measured in seconds and bytes, so performance claims are
held to the same bar as correctness claims.

- A change that claims a speed-up or a size reduction includes the
  `bench/bench.py` numbers before and after, on a named machine, in the pull
  request. "Should be faster" is not a result.
- Commit new result JSON from `bench/results/` together with the output of
  `python3 bench/report.py`, which regenerates the tables in
  `bench/README.md` and the SVG charts. Never edit those tables by hand.
- Record what moves the number. Linker results in particular swing with
  filesystem, output reuse, fork mode, and thread count; the harness writes the
  machine into every result file, so do not strip it.
- Quote third-party numbers only with a link to their source, and label them
  as not reproduced by tinymold.
- The harness uses only the Python standard library. Keep it that way so it
  runs on a bare machine.

## Cache Correctness

A cache that is fast and occasionally wrong is worse than no cache.

- Never key on anything that is not an input (mtimes, absolute workspace paths,
  terminal flags), and never omit anything that is (see
  [`docs/specs/cache-key.md`](docs/specs/cache-key.md)).
- Objects in the store are immutable. Never write to a materialized hardlink;
  anything a tool rewrites in place must be restored by reflink or copy.
- When in doubt, miss. An uncacheable action that runs is a slowdown; a wrong
  hit is a miscompile.

## Git Workflow

- Never commit directly to `main`. Branch first, one branch per logical change.
- Do feature work in a git worktree so the main checkout stays clean.
- Commit subjects are concise and imperative: `Add retry policy to the client`.
  Keep the subject specific to the change and under ~72 characters.
- Make small, focused commits. Each commit should cover one logical change,
  build independently, and avoid mixing formatting, refactors, and behavior
  changes unless they are inseparable.
- Never commit secrets. `.env` is git-ignored; document new variables in
  `.env.example` with placeholder values.
- Never force-push a shared branch, rewrite published history, or bypass hooks
  with `--no-verify`.

## Pull Requests

Open pull requests ready for review, not as drafts, unless the work genuinely
must not merge yet. A pull request should:

- summarize what changed and why, in a few sentences;
- call out public API or behavior changes explicitly, or state "None";
- list the validation commands actually run, with their outcome;
- link the related issue;
- include updated tests, docs, and examples in the same change.

The template in `.github/PULL_REQUEST_TEMPLATE.md` encodes this checklist.
Address review feedback by fixing it, and reply on each thread describing what
changed. Do not resolve a thread whose feedback you have not addressed or
explicitly declined with a reason.

## Releases

Releases run from `.github/workflows/release.yml` via a manual
`workflow_dispatch` with a `patch` / `minor` / `major` bump. `current` releases
the version already in `Cargo.toml` when it has no tag yet (this is how 0.0.1
ships), or resumes an interrupted release whose tag exists. The workflow
re-runs the full validation suite, bumps the root `[workspace.package]`
version, the internal `version = "=X.Y.Z"` requirements, and `Cargo.lock`,
commits and tags `vX.Y.Z`, builds the `tinymold` binary for Linux, macOS, and
Windows on x86_64 and ARM64, creates an immutable GitHub release with
`SHA256SUMS`, and publishes every crate to crates.io leaf-first using the
`CARGO_REGISTRY_TOKEN` secret of the `Production` environment. Last, it moves the major tag (`v0` for 0.x) to
the new release, because workflows pin the GitHub Action as
`tinyhumansai/tinymold@v0`; it is the only tag that ever moves.

Consequently:

- Do not hand-edit the `version` field in the root `[workspace.package]` or the
  internal dependency requirements; the release workflow owns both. Every
  member inherits the workspace version, so the whole workspace releases as one
  version.
- Follow semantic versioning. Any change to the public surface that is not
  purely additive is a breaking change and needs a major bump (pre-1.0: a minor
  bump).
- crates.io versions are permanent. Never run `cargo publish` by hand; CI's
  `cargo package --workspace` step is how you check a crate would publish.
- Every release target must build — `main` should always be green.

## Agent Working Agreement

For automated contributors specifically:

1. **Read before writing.** Inspect the surrounding module and match its
   conventions, comment density, and idiom rather than importing a house style.
2. **Verify, do not assume.** Run the four contract commands and read their
   output before reporting a task complete. Report failures with the output;
   never claim a check passed that you did not run.
3. **Stay in scope.** Implement what was asked. Do not opportunistically
   refactor, reformat, upgrade dependencies, or "fix" unrelated code — raise it
   instead.
4. **No placeholders in delivered code.** No `todo!()`, no stubbed functions, no
   commented-out alternatives left behind. If something cannot be finished, say
   so explicitly.
5. **Do not weaken the guardrails.** Never add blanket `#[allow(...)]`, relax a
   lint, mark a test `#[ignore]`, or loosen CI to get a green run. Fix the
   cause.
6. **Secrets stay out.** Never read, echo, or commit `.env` contents, tokens, or
   credentials, and never paste them into a pull request or issue.
7. **Ask only when blocked.** Make routine judgment calls yourself; escalate
   only irreversible decisions or genuine forks with no clear default.
