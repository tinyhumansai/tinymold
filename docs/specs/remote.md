# The remote tier

- **Status:** Implemented
- **Owner:** Steven Enamakel
- **Date:** 2026-10-14
- **Plan:** [`../plans/remote-and-ci.md`](../plans/remote-and-ci.md)
- **Builds on:** [ADR 0006](../adr/0006-remote-tier-reapi-bazel-remote.md),
  [ADR 0002](../adr/0002-blake3-for-content-digests.md),
  [`rustc-wrapper.md`](rustc-wrapper.md)

## Problem

A fresh runner, or a new developer machine, starts with an empty store. It
then compiles every unit, even though another machine compiled the same
units from the same inputs an hour ago.

## Goals and non-goals

Goals:

- On a local miss, fetch the unit from a shared REAPI cache before running
  rustc.
- Upload a job's new entries in one batch at the end of the job, not on the
  hot path.
- Authenticate with a credential stored as a CI secret. Allow read-only
  clients.
- Never let a remote failure or a remote lie break or corrupt a build.

Non-goals:

- Remote execution.
- Shipping a server. That is now [`cache-server.md`](cache-server.md)
  ([ADR 0009](../adr/0009-a-cache-server-with-protocol-front-ends.md)); this
  client works against it and against any other REAPI cache.
- Uploading from the wrapper on every miss.

## Configuration

| Variable | Meaning |
|---|---|
| `TINYMOLD_REMOTE_URL` | `grpc://host:port` (plaintext) or `grpcs://host[:port]` (TLS). Unset: no remote tier |
| `TINYMOLD_REMOTE_AUTH` | `user:password`, sent as HTTP basic auth on every call. Unset: anonymous |
| `TINYMOLD_REMOTE_INSTANCE` | REAPI instance name; default empty |
| `TINYMOLD_REMOTE_TIMEOUT` | Per-call timeout, as a `tinymold gc` age (`2s`); default `5s` |
| `TINYMOLD_REMOTE_SIGNING_KEY` | A writer's ed25519 secret key, 64 hex digits. `push` signs every entry with it |
| `TINYMOLD_REMOTE_TRUSTED_KEYS` | Comma-separated ed25519 public keys, 64 hex digits each. When set, an entry not signed by one of them is a miss |

`tinymold doctor` shows whether a remote is configured. It never prints the
credential.

## Data model

Two kinds of ActionCache entries, both holding ordinary REAPI
`ActionResult`s:

- **Action.** The AC key is the unit's full key. Its `output_files` are:
  - `.tinymold/action`, the tinymold action record;
  - one entry per object the record references (outputs, stdout and
    stderr), each with the object's BLAKE3 digest as its path and its
    SHA-256 as its digest.

  Names and modes stay in the record. Listing every object makes servers
  that check AC dependencies (bazel-remote does) keep, and vouch for, every
  blob the entry needs.
- **Input sets.** The AC key is `BLAKE3(field "tinymold/remote/prekey/v1",
  pre-key)`. Its one output file, `.tinymold/prekey`, is the pre-key record
  text.

**Digests.**
- **CAS blobs** use SHA-256, computed by the client when it uploads.
- **AC keys** are tinymold's 32-byte BLAKE3 digests, sent with
  `size_bytes = 32`. Servers treat them as opaque, but bazel-remote refuses
  a size of 0 for any hash but the empty blob's.

The salt on the pre-key entries keeps the two kinds of entry from ever
sharing a key.

## Lookup (in the wrapper, on a local miss)

1. Read the remote input sets for the pre-key. If there are none, it is a
   miss.
2. Merge them into the local pre-key record. Remote input identities are
   zeroed, which forces a rehash. Then try each set as the local lookup
   would.
3. For the first full key the local store lacks, get the remote action:
   1. fetch every listed object the local store does not hold, refusing a
      blob whose SHA-256 is wrong;
   2. insert each into the local CAS, which re-derives its BLAKE3 digest,
      and refuse the entry when that digest is not the name it was listed
      under;
   3. fetch and decode `.tinymold/action`, and check that every digest it
      names is now in the store; if any is missing, it is a miss.
4. Write the local action and restore it, as a local hit would. The log
   outcome is `remote`.

Calls go through ByteStream `Read` for blobs, so blob size is not limited
by the gRPC message size. The connection uses HTTP/2 adaptive flow-control
windows, so reads from a distant cache are not held to a fixed window per
round trip. Once an entry's signature checks out, its
objects and its record are read together, within 32 concurrent calls and
a 32 MiB byte budget, and a pre-key
lookup reads its signature and record together: each read is a round trip,
and a runner far from its cache would otherwise pay one per object on every
hit. Each call has the configured timeout, and blob
transfers get one more second per 4 MiB. **Any
error, timeout or verification failure is logged and becomes a miss**, and
rustc runs.

## Push (`tinymold remote push`)

```text
tinymold remote push [--since <unix-seconds>]
```

1. Gather local actions, and pre-key records, whose last use is at or after
   `--since` (default: all).
2. Check existing remote actions concurrently before reading local outputs.
   An entry with the same record, output names, and writer signature is
   refreshed by `GetActionResult` (which counts as a use on tinymold and
   bazel-remote). Check its dependencies with `FindMissingBlobs` too. Missing,
   incomplete, malformed, or differently signed entries take the upload path.
   No result returned by the remote is re-signed or republished.
3. For actions that need uploading, compute the SHA-256 of each distinct local
   object once per push, and ask `FindMissingBlobs` for them in batches.
4. Upload the missing blobs with ByteStream `Write`, then `UpdateActionResult`
   for the action. Pre-key entries go last, so a reader that finds an input
   set can find its action.

Up to 32 uploads, and at most 256 MiB of blobs, are in flight at once (a
larger blob goes alone), then up to 32 entry updates, every action's before
any pre-key's. The byte budget bounds the memory a push holds, and because
the transfers share the link, each one's deadline allows for the whole
256 MiB beside it at the 4 MiB/s floor: 64 s more than a transfer alone.
Downloads keep their separate 32 MiB budget, since many compiler wrappers
may run at once.
One at a time, each call would wait out a full round trip to the server.

The command reports how many entries and bytes it uploaded, how many existing
actions it refreshed, and the refresh, preparation, and transfer phase times. It fails, with
exit 1, on the first rejected write, such as missing write credentials.

A call that times out, or that the server answers as `UNAVAILABLE`,
`DEADLINE_EXCEEDED`, `RESOURCE_EXHAUSTED` or `ABORTED`, is retried after 1 s,
4 s and 15 s. So is a call cut off by an HTTP/2 GOAWAY (tonic reports it as
an `INTERNAL` "h2 protocol error"): a TLS proxy in front of the server, such
as nginx, closes a connection after its per-connection request limit, and
the channel reconnects for the retry. A 502, 503 or 504 from such a proxy
(tonic reports an `INTERNAL` error over its non-gRPC body) is retried too:
nginx answers that way while the server behind it restarts. A server sharing a saturated disk with the builds can stall a
write for seconds, and before the retries one stall threw the whole push
away: blobs went up, but no action did. A blob that still fails leaves out
only the entries that name it; the report counts them as "not pushed after
upload failures". The push exits 1 when those failures left no action or
no pre-key record pushed, since readers find actions only through pre-keys.

`tinymold remote check` calls `GetCapabilities`, then uploads a small probe
blob. It reports the server's digest functions and whether the client can
write, and exits 1 when the server is unreachable or a configured credential
is refused. The probe matters because a server that allows unauthenticated
reads answers `GetCapabilities` whatever the credential.

## Trust

- Only trusted CI holds a write credential. Pull requests from forks get no
  secrets, and read anonymously when the server allows unauthenticated
  reads, or run without a remote.
- **A remote can only make a build miss.** Everything it returns is
  re-hashed and checked against digests in records the client parsed itself.
  A server that returns a wrong record for a key is trusted as much as the
  CI that wrote it, which is why writes are restricted, and why readers
  should name trusted keys (see [Signed entries](#signed-entries)) so that
  a misconfigured server cannot change that.

## Signed entries

Restricting writes protects a cache only while the server is configured
right. One bad bind address or a missing credential file lets anyone who
can reach the server plant an entry that decides what a later build links.
Signed entries make the client independent of that: a reader that names
its trusted keys uses only entries those keys signed, so a server open to
the world can at worst make a build miss.

- **Keys.** `tinymold remote keygen` prints a fresh ed25519 key pair as the
  two variables above. The secret goes only to trusted writers, such as CI
  on pushes to `main`; the public key goes to every reader, including
  pull-request jobs, which therefore cannot sign.
- **What is signed.** Each `ActionResult` gains a third kind of output file,
  `.tinymold/signature`, whose blob is

  ```text
  tinymold-signature 1
  key <public key, 64 hex>
  sig <ed25519 signature, 128 hex>
  ```

  over the message

  ```text
  tinymold/remote/entry/v1
  <action | prekey>
  <ActionCache key hash>
  <record blob SHA-256>/<record size>
  ```

  The record names every object by BLAKE3, and lookup already refuses an
  object whose content does not match its name, so signing the record's
  digest under its key binds the whole entry. The kind line keeps an action
  signature from validating a pre-key entry and the other way round.
- **Verification** happens before any object is fetched. With
  `TINYMOLD_REMOTE_TRUSTED_KEYS` set, an entry with no signature, a
  malformed one, an untrusted key or a bad signature is refused
  (`Error::Untrusted`), which the wrapper logs and turns into a miss.
  Without it, entries are used unsigned, as before.

`remote check` also probes the server anonymously. A server that accepts an
anonymous write while this client trusts no keys is refused with
`Error::OpenToAnonymousWrites` and exit 1: anyone who can reach it could
poison the cache. Either require a credential for writes or configure
trusted keys.

## Acceptance criteria

- With a populated remote and an empty local store, a clean build of this
  workspace reaches ≥ 90% `remote` outcomes (CI `remote-e2e` job).
- An unreachable remote costs at most one timeout per wrapper and the build
  succeeds.
- A blob whose content does not match its digest is rejected, and the unit
  compiles.
- A wrong credential makes `remote check` and `remote push` exit 1 and
  leaves builds working.
- With trusted keys set, an unsigned, forged or foreign-signed entry is a
  miss and none of its objects is fetched; an entry signed by a trusted key
  is a hit.
- `remote check` exits 1 against a server that accepts anonymous writes,
  unless trusted keys are set.
