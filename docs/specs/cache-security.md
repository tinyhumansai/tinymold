# Cache security for public repositories

## Trust boundary

A cache writer can influence code that a later build executes. A malicious
artifact can have valid SHA-256 and BLAKE3 digests: hashes prove which bytes
arrived, not that those bytes came from the expected compiler or source.
Cached executables, build scripts, proc macros and libraries can introduce
code into a build or its resulting release. A poisoned test-result record
can also suppress a test run.

Uploading bytes through the cache protocols does not ask tinymold's server
to execute them. Turbo, Nx and Gradle artifacts remain opaque blobs; the
server does not extract their archives. Their consumers remain responsible
for safe extraction and for trusting the producer. REAPI execution is not
implemented. These properties do not make arbitrary uploads harmless:
storage, bandwidth, parsing and concurrent requests still use resources.

## Required deployment policy

- Give write access only to CI that builds trusted code. Fork PRs, ordinary
  PR jobs and other untrusted workloads get read-only credentials or no
  credential. A repository being public does not make its contributors
  trusted cache writers.
- For the Rust REAPI adapter, configure `TINYMOLD_REMOTE_TRUSTED_KEYS` on
  every reader. Keep `TINYMOLD_REMOTE_SIGNING_KEY` only in the trusted
  producer job. Both pre-key and action records must be authenticated.
  Signed entries are currently opt-in; an unsigned configuration does not
  provide this guarantee. A signature authenticates the producer, not the
  correctness of its build.
- Scope credentials by protocol and repository tenant. Use distinct signing
  keys for repositories or groups that must not trust each other's builds.
  The current signature binds the action key and record, but not the REAPI
  instance name: a shared key is a shared signing trust domain.
- Turbo, Nx, Gradle and ordinary Bazel clients do not gain the Rust
  adapter's signature verification. Restrict their write credentials and
  isolate their trusted and untrusted namespaces server-side.
- Use TLS across untrusted networks. Do not put credentials or sensitive
  artifacts in a cache readable by public PRs. The server's CAS digest
  index is shared across protocols and tenants; namespaces isolate action
  mappings, not confidential blob storage. Use separate server stores for
  confidentiality boundaries.
- Run untrusted jobs on disposable runners. The local wrapper store trusts
  local records and objects; remote signature verification does not protect
  a store an attacker can modify directly. Never mount a trusted persistent
  store writable into an untrusted job. An untrusted process on the same
  runner can also tamper with restored outputs and other build state.
- A job holding a signing key must not consume unsigned or untrusted local
  cache state and then publish it. Start with a clean store when changing
  signing policy or rotating away from a contaminated trust domain. Signed
  remote verification is performed on fetch, not on every subsequent local
  hit. Existing local entries are not retroactively authenticated.

Workflow conditions are useful defaults, but malicious code given a write
credential can bypass a save step and call the API itself. Enforce read-only
access on the server and withhold signing secrets at the workflow boundary.
Do not execute PR code in a privileged `pull_request_target` job.

## GitHub Actions

GitHub scopes its own cache separately from a shared remote server. A normal
`pull_request` cache is scoped to its merge ref; a trusted main build must
not import that archive through another mechanism. GitHub documents its
[cache access restrictions and low-trust trigger policy](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).
Those restrictions do not apply to a persistent self-hosted runner directory
or an independent REAPI cache. Do not override low-trust cache restrictions
to let untrusted jobs populate trusted caches.

For public PRs consuming a shared Rust remote, use a fresh runner/store,
the public verification keys, and read-only server access. For example:

```yaml
- uses: tinyhumansai/tinymold@v0
  with:
    backend: remote
    remote-url: grpcs://cache.example.com
    remote-instance: my-repository
    remote-trusted-keys: ${{ vars.TINYMOLD_REMOTE_TRUSTED_KEYS }}
    # Anonymous reads, or a server-enforced read-only credential.
- run: cargo test --all-features
- uses: tinyhumansai/tinymold/save@v0
  if: always()
  with:
    push: 'false'
```

Configure trusted producers in a separate workflow building trusted refs,
with both the signing secret and the same public verification keys. Do not
pass a writer credential or signing key to the public PR workflow. This
example requires a release containing the protections described below;
`v0` alone does not identify a particular audited commit.

## Upload and restore protections

| Boundary | Protection |
| --- | --- |
| Server upload | Streamed into private staging; content-derived names; claimed digests checked before committing |
| REAPI batch reads | Declared total limited to 4 MiB with overflow checks; actual stored sizes checked before bounded reads |
| Server network access | Token scopes distinguish read and write; an unauthenticated listener is restricted to loopback |
| Remote artifact reads | SHA-256 verified on receipt, BLAKE3 verified on insertion |
| Remote authorship | Ed25519 verification of records when trusted keys are configured |
| Remote metadata | Action/pre-key records limited to 16 MiB; signatures to 1 KiB, checked before downloading |
| Remote object list | Parsed action record must authorize exactly the distinct listed objects; extra, duplicate or missing objects refuse the entry before artifact downloads |
| Output filenames | Action parser rejects absolute paths, directory separators, drive separators, `.` and `..` |
| Text restore | Creates a new destination, refusing an existing file or symlink instead of truncating or following it |
| Binary restore | Reflink or exclusive-create copy, rather than a writable hardlink to the CAS |
| Incremental restore | Session destination must be a direct child of the invocation's incremental directory |

The text-destination protection assumes the surrounding output directory is
trusted. It does not claim to defeat a malicious local process concurrently
replacing parent directories or modifying files after restoration.

## Remaining availability and assurance limits

`--max-body` bounds an individual upload (4 GiB by default), not the total
number of simultaneous uploads. `--max-size` triggers periodic eviction;
it is not a hard admission quota. The Rust client can still buffer one
artifact up to its 4 GiB limit even though ordinary concurrent transfers use
a smaller byte budget. A malicious authorized writer or server can cause
disk, memory, CPU or network pressure within and across these limits.
Use service isolation, filesystem quotas and ingress rate/concurrency limits
when exposing the service to untrusted traffic. There is no claim of zero
availability impact from public uploads.

This is a source review of upload, remote-fetch and Rust restore boundaries,
with regression tests for the identified gaps. It is not a complete security
audit of every parser, third-party consumer, operating-system race or CI
deployment. No production credentials or repository workflows were changed
as part of this hardening.
