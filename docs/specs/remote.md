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
by the gRPC message size. Each call has the configured timeout, and blob
transfers get one more second per 4 MiB. **Any
error, timeout or verification failure is logged and becomes a miss**, and
rustc runs.

## Push (`tinymold remote push`)

```text
tinymold remote push [--since <unix-seconds>]
```

1. Gather local actions, and pre-key records, whose last use is at or after
   `--since` (default: all).
2. For each action, compute the SHA-256 of every blob it needs, and ask
   `FindMissingBlobs` for them in batches.
3. Upload the missing blobs with ByteStream `Write`, then `UpdateActionResult`
   for the action. Pre-key entries go last, so a reader that finds an input
   set can find its action.

The command reports how many entries and bytes it uploaded. It fails, with
exit 1, on the first rejected write, such as missing write credentials.

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
  CI that wrote it, which is why writes are restricted.

## Acceptance criteria

- With a populated remote and an empty local store, a clean build of this
  workspace reaches ≥ 90% `remote` outcomes (CI `remote-e2e` job).
- An unreachable remote costs at most one timeout per wrapper and the build
  succeeds.
- A blob whose content does not match its digest is rejected, and the unit
  compiles.
- A wrong credential makes `remote check` and `remote push` exit 1 and
  leaves builds working.
