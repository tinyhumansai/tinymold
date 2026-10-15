# 10. Default-branch builds anchor eviction

- **Status:** Proposed
- **Date:** 2026-10-15
- **Builds on:** [ADR 0005](0005-trade-disk-for-time.md),
  [ADR 0009](0009-a-cache-server-with-protocol-front-ends.md),
  [`gc.md`](../specs/gc.md), [`cache-server.md`](../specs/cache-server.md)

## Context

`tinymold serve` holds every language's artifacts in one store under one
byte budget (ADR 0009). The spec plans eviction as LRU by entry, with an
optional age limit, the same policy `tinymold gc` applies to the local store.
ADR 0005 keeps budgets as a last resort: disk is cheap, and below the budget
nothing should be removed.

LRU treats every entry alike, but entries are not alike. A repository's
default branch (`main`) is the base every later build starts from. A pull
request branches from it, so most of its keys are `main`'s keys, and the
entries that serve them are the ones `main` last built or hit. An entry that
only one feature branch ever produced serves that branch and, once it merges
or is abandoned, nothing.

Under pressure, LRU gets this wrong in two ways:

- **A burst of branch builds evicts `main`'s working set.** Twenty pull
  requests on a busy afternoon each write their own entries, and those are
  the most recently used. When the store reaches its budget, the entries
  that go are the ones `main` wrote this morning. Every next pull request
  then misses on work that has not changed.
- **Superseded `main` entries linger.** After `main` moves on, the entries
  only its previous commit used stay until they age out, though no future
  build will ask for them.

Clients do not send their branch. Turborepo, Nx, Gradle, ccache, Bazel and
the Rust adapter each send a key and, at most, a token. Whatever marks a
build as `main`'s has to ride on something every protocol already carries.

## Decision

The store keeps a **reference set** for each repository's default branch: the
entries its most recent builds touched. Under pressure, eviction removes
everything else first. Below the budget, nothing changes.

### Reference sessions

- A build of the default branch opens a **reference session** with a label
  and a commit, for example `tinyhumansai/tinymold@main` at `4711d3b`:
  `tinymold serve session begin --label <label> --commit <sha>`, an admin
  call made with a write token.
- The server answers with a **session token**, scoped like the token that
  minted it and no wider. The build uses it in place of its usual token.
  Every front-end already accepts a token (`TURBO_TOKEN`, Nx's bearer token,
  basic auth for Gradle, REAPI and Bazel HTTP, the native socket's session),
  so no protocol changes and no client needs to know about branches.
- Every entry the session token **reads or writes** joins the session's set.
  Reads count: when `main` hits an entry a pull request wrote, that entry
  joins `main`'s set instead of being rebuilt. This is the repoint: the
  reference moves to whatever `main` actually used, wherever it came from.
- `tinymold serve session end --commit-set` commits the set. A session that
  is abandoned, fails, or outlives its timeout commits nothing. A partial
  set never becomes a root.

### Roots

- The newest `--keep-references <n>` committed sets per label (default 2)
  are **roots**. An entry in a root is evicted only under the last rule
  below.
- A newly committed set supersedes the oldest kept one. Entries that only a
  superseded set held become ordinary entries again, evicted by LRU like any
  branch entry. That is the refresh: each `main` build re-anchors the cache
  on what it wrote and used, and lets the rest go.
- A set records store keys only (32 bytes each), so a 100 000-entry build
  costs about 3 MB per set. The encoding is versioned and pinned by a
  golden-vector test.

### Eviction under pressure

Collection starts when the store passes a high watermark of the budget
(default 100% of `--max-size`) and stops at a low watermark (default 90%).
It removes entries in this order, each step LRU within itself:

1. Entries in no root and unused for longer than `--branch-max-age`.
2. Entries in no root.
3. Entries held only by a kept but not newest set, oldest set first.
4. Never an entry in a label's newest set. If those alone exceed the budget,
   the server logs it, reports it in `/metrics` and `serve status`, and
   refuses new entries outside a root until an operator raises the budget or
   purges. The budget is then too small for the repository, and evicting
   `main`'s current state would make every build miss.

Objects follow entries as in [`gc.md`](../specs/gc.md): an object is freed only
when no surviving entry references it, younger objects keep their grace
period, and in-flight uploads stay pinned.

### Manual repoint

`tinymold serve purge --keep-references` removes every entry that is in no
root, at once, regardless of the watermarks. It is the reset for a store that
has blown past its budget: what survives is exactly what the latest `main`
builds need, and the next builds refill the rest with fresh entries.

### In GitHub Actions

With `serve: true`, the setup action opens a reference session when
`github.ref` is the default branch, and the save action commits it before
running GC to the budget and saving the store. Pull request jobs use plain
tokens and restore what `main` saved. This turns the current advice in
[`github-actions.md`](../specs/github-actions.md), to save the cache only
from the default branch, into something the store itself enforces.

## Consequences

- **Pull requests keep hitting under pressure.** The entries they share with
  `main` are the last to go, so a busy day of branch builds evicts branch
  entries rather than the base every branch needs.
- **The store shrinks toward what matters.** Each `main` build releases the
  entries only its predecessors used, so a store under pressure converges on
  the last `n` builds of each default branch plus recent branch work.
- **No new correctness risk.** Collection only deletes, and an evicted entry
  is a miss. No branch, label or commit enters any key or entry record; the
  reference set is bookkeeping beside the index.
- **Cost on the hot path.** A request under a session token appends one store
  key to the session's set. Other requests pay nothing. Committing a set is
  one file write at the end of a build.
- **Operators name the reference.** The server cannot discover which branch
  is the default; the CI that runs on it says so by opening a session.
  Without sessions, eviction is plain LRU, as the spec describes today.
- **One more scope in the token model.** A session token is a write token
  with a set attached. It can never grant more than the token that minted it.
- **The local store is unchanged for now.** `tinymold gc` keeps its policy.
  The same sets could later back a `--keep-references` flag there.

### Alternatives rejected

- **Tag requests with a branch header.** Clients have no setting for it, and
  a front-end that invented one would work only with patched clients.
- **A separate store or tenant per branch.** It loses the cross-branch dedup
  and the pull-request-to-`main` reuse that make one store worth running.
- **Plain LRU with a larger budget.** It postpones the problem. The first
  burst of branch builds that fills the budget still evicts `main`'s set.
- **Pin `main`'s entries forever.** The store would only grow; a reference
  that never moves keeps every superseded build.
