# tinymold-link: a stateful fork of mold

- **Status:** Draft
- **Owner:** Steven Enamakel
- **Date:** 2026-10-15
- **Builds on:** [ADR 0009](../adr/0009-fork-mold-into-a-stateful-linker.md),
  [`mold-and-bun.md`](../research/mold-and-bun.md),
  [ADR 0002](../adr/0002-blake3-for-content-digests.md),
  [`architecture.md`](architecture.md)
- **Plan:** [`../plans/linker.md`](../plans/linker.md)

## Problem

A Rust edit loop relinks after almost every edit, and each link starts from
nothing. mold opens and parses every member of every archive it is given,
splits and hashes every mergeable section, and writes the whole output again
([mold-and-bun §1.3](../research/mold-and-bun.md#13-the-pipeline-and-what-repeats-every-link)).
Between two links of the same binary, std and every dependency rlib are
usually byte-identical, so most of that work produces the same answer it
produced last time.

`cargo test` makes it worse: each test target is its own binary, linked
against the same dependencies, often in parallel.

## Goals and non-goals

Goals:

1. A linker, `tinymold-link`, that can be used wherever mold is used, on ELF
   (x86_64, AArch64) and Mach-O (arm64, x86_64).
2. With no cache configured, output identical to mold at the fork point.
3. A persistent per-input parse cache that removes parsing of unchanged
   archive members and object files from every link after the first.
4. Measurement built in, so every claimed win comes with a per-pass split.

Non-goals:

- Faster cold links than mold. The fork does not tune passes mold already
  runs in parallel.
- Targets other than the four above until someone needs them.
- A resident daemon, merge-fragment caching, append linking and incremental
  output in this spec's first version; they are later phases of the
  [plan](../plans/linker.md) and get their own spec sections when they start.
- Non-reproducible output. Every cache must leave the output byte-identical.

## Invocation

`tinymold-link` accepts mold's command line unchanged. It selects the backend
the same way mold does:

| Platform | How it is reached | Backend |
|---|---|---|
| Linux, ELF | `-C link-arg=-fuse-ld=/path/to/tinymold-link` with gcc or clang, or `-C link-arg=--ld-path=/path/to/tinymold-link` with clang | ELF |
| macOS, Mach-O | an `ld64.tinymold` name for the same binary (a hardlink or symlink), passed as `-C link-arg=-fuse-ld=/path/to/ld64.tinymold` | Mach-O, because argv0 starts with `ld64` |

Example cargo configuration on Linux:

```toml
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=--ld-path=/usr/local/bin/tinymold-link"]
```

The version banners keep mold's prefixes, because build tools and mold's own
tests recognize the linker by them, and name the fork after them:
`tinymold-link --version` prints `mold 3.0.0 (tinymold-link 0.0.1; compatible
with GNU ld)`, the same string goes into `.comment` on ELF (so `readelf -p
.comment` shows which linker ran), and `ld64.tinymold -v` prints `mold-macho
3.0.0 (tinymold-link 0.0.1)`. The upstream commit is recorded in
[`crates/tinymold-link/FORK.md`](../../crates/tinymold-link/FORK.md).

mold's own measurement options are kept: on ELF, `--perf` prints the per-pass
wall and CPU time tree and `--stats` prints counters (`parsed_objs` and the
cache counters below); on Mach-O, ld64's `-print_statistics` prints the same
tree.

## The parse cache

### Enabling it

The cache is off unless `TINYMOLD_LINK_CACHE` is set to a tinymold store root
(the same directory `TINYMOLD_STORE` names). An empty value is off. The
command-line option `--tinymold-cache=<dir>` overrides the variable for one
link; the variable is the primary interface because cargo passes linker
arguments through the compiler driver, which is awkward to vary per build.

If the store cannot be opened, the link proceeds without the cache and prints
one warning; it never fails because of the cache.

### What is cached

For each ELF relocatable object (a standalone `.o` or an archive member) and
each Mach-O object, the cache holds the **file-local** result of parsing it:

- the symbol table: name, binding, type, visibility, section index, value,
  size, and whether the symbol is defined, common or undefined;
- the section headers needed to decide liveness and COMDAT membership:
  index, name, type, flags, size, alignment, and COMDAT group signatures;
- the input's machine type, so a mismatch is reported exactly as mold reports
  it.

It does not hold anything that depends on other inputs: global symbol IDs,
file IDs, resolution results, or the relocation rewrites mold applies inside
its private input mappings. Those are recomputed every link from the cached
file-local state.

### Key

An entry is found by its **content key**:

```text
content key = BLAKE3("tinymold/link/parse/v1" ‖ digest(member bytes))
```

where `digest` is `tinymold-hash`'s BLAKE3-256 `Digest` and fields are
length-prefixed with its field hasher. Paths, archive names and mtimes are not
part of the key, so the same member in two worktrees, or the same std object
in two projects, shares one entry.

Each entry also records an **options hash** that is checked on load rather
than used to find the entry:

```text
options hash = BLAKE3(
  cache format version,
  tinymold-link version and upstream mold commit,
  backend (ELF or Mach-O) and target machine,
  every argument that changes what parsing produces
)
```

The arguments that change parsing are listed in one place in the code, next
to the code that reads them, and a test fails if a parsing-related argument is
added without being listed. Phase L2 derives the list by reading what
`ObjectFile::new` and `register_global_symbols` consult at the fork point;
the candidates are `--exclude-libs`, `--wrap`, `--strip-debug`, `--strip-all`
and the emulation (`-m`). Until an argument is shown not to matter, it is
included.

Two hashes instead of one follows Bun's transpiler cache
([mold-and-bun §2.1](../research/mold-and-bun.md#21-two-hash-cache-entries)): the
entry name stays a pure function of the bytes, and a flag change makes the
entry a miss that is then replaced.

### Entry format

An entry is one immutable object in the tinymold CAS, read with one `pread`
into an uninitialized buffer and validated before any field is used.

```text
offset  size  field
0       16    magic "tinymold-parse\0\0"
16      4     format version (u32 LE), starts at 1
20      4     header size (u32 LE)
24      32    content key
56      32    options hash
88      8     input length in bytes (u64 LE)
96      ...   column directory: for each column, offset (u64) and length (u64)
...           columns, each 8-byte aligned
...           string table
```

- **Header.** `#[repr(C)]`, little-endian on disk regardless of host, with
  explicit padding fields and a compile-time size assertion.
- **Columns.** The symbol table and section table are stored as
  struct-of-arrays columns (one column per field), each a packed array of a
  fixed-width integer type. Reading one is a bounds check and a slice cast.
- **Strings.** Symbol and section names are stored as an offset and length
  into one string table. The writer runs a counting pass over all names
  first, allocates the table once, then fills it.
- **Validation before use.** The reader checks the magic, version, header
  size, that the content key matches the key it looked up, that the options
  hash matches the current link, that the input length matches, that every
  column offset and length lies inside the entry and is aligned, and that every
  string offset lies inside the string table. Enum fields (binding, type,
  visibility) are range-checked. Any failure is a miss.

The format is pinned by a unit test against a known byte vector. Changing the
encoding requires bumping the format version, which changes every options hash
and so turns every old entry into a miss.

### Storage

- Entries are stored as CAS objects with `Store::insert_bytes`, so they are
  immutable, named by their own digest, verified on insert and covered by
  `tinymold gc`.
- An index maps a content key to the digest of its entry:
  `link/parse/<first 2 hex>/<remaining 62 hex>` under the store root, a
  32-byte file written to a temporary name and renamed into place.
- Inputs smaller than a minimum size are not cached. The threshold starts at
  4 KiB (Bun's measured threshold for its transpiler cache) and is tuned from
  phase L1's measurements: below it, opening the index and the entry costs
  more than parsing.

### Lookup, hit, miss and store

For each input the linker would parse:

1. If the input is under the size threshold, parse it as mold does.
2. Hash the bytes (already mapped or read by mold) and compute the content key.
3. Read the index file; if it is missing, it is a miss.
4. Read and validate the entry; if validation fails, it is a miss, and the
   index file is removed so the next link does not retry it.
5. On a hit, build the in-memory object from the entry and continue with
   symbol registration exactly as after a parse.
6. On a miss, parse as mold does, then encode the file-local result and store
   it. Stores happen off the critical path and never fail the link.

`--stats` reports `parse_cache_hits`, `parse_cache_misses`,
`parse_cache_rejects` (entries found but invalid) and `parse_cache_stores`.

## Invariants

- **When in doubt, miss.** Any error, mismatch or unexpected value in the
  cache path falls back to a full parse.
- **Byte-identical output.** For the same inputs and arguments, the output
  with the cache enabled is byte-identical to the output with it disabled,
  whether every lookup hits, misses or is rejected.
- **No path or time in the key.** The content key depends only on the bytes
  of the input; the options hash depends only on the arguments, the target and
  the linker version.
- **Immutable entries.** Entries are never rewritten. A newer encoding gets a
  new format version.
- **The cache never fails a link.** Store errors, full disks and unreadable
  indexes print at most one warning per link.
- **No cache, no change.** With `TINYMOLD_LINK_CACHE` unset, `tinymold-link`
  behaves as mold at the fork point, and mold's own test suite passes.

## Acceptance criteria

- `tinymold-link` links ripgrep and bat on darwin-arm64 (Mach-O) and on
  x86_64 Linux (ELF), and the binaries run their own test suites.
- The mold shell-test subset run in CI passes for both backends.
- Linking a fixture binary three ways (cache off; cache on with an empty
  store; cache on with a warm store) produces byte-identical outputs.
- A tampered entry, an entry with a different format version and an entry with
  a different options hash each produce a miss and a correct link, and are
  counted as rejects.
- The bench harness records cold and warm-cache link times for ripgrep and bat
  with `--perf` splits. A speed-up is claimed only from those numbers.

## Open questions

- Is a cached symbol table cheaper to load and re-intern than re-parsing an
  mmapped ELF symbol table? Phase L1 measures parse time per member before the
  cache is built; if parsing is already cheap, the entry must also cover
  section metadata or merge fragments to pay off.
- Should the index live in the CAS's `entries/` layout from
  [ADR 0004](../adr/0004-store-layout-aligned-with-cargo.md) instead of a new
  `link/parse/` directory? `tinymold gc` must learn about whichever is chosen.
- Rust rlibs carry `lib.rmeta` as a member. Skipping it before hashing would
  save a hash per rlib; it needs a check that no linker input ever references
  it.
