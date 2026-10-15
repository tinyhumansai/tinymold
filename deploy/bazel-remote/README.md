# Running a remote cache for tinymold

tinymold's remote tier talks REAPI v2 to an existing cache server
([ADR 0006](../../docs/adr/0006-remote-tier-reapi-bazel-remote.md)). This
directory runs [bazel-remote](https://github.com/buchgr/bazel-remote), the
reference server, from its published image on a local persistent disk.
How the client uses it is in [`docs/specs/remote.md`](../../docs/specs/remote.md).

| File | Use |
|---|---|
| [`docker-compose.yml`](docker-compose.yml) | One VM or a developer machine |
| [`k8s/bazel-remote.yaml`](k8s/bazel-remote.yaml) | A StatefulSet with a persistent volume, and its Service |
| [`k8s/ingress.yaml`](k8s/ingress.yaml) | TLS and a public host name at an ingress-nginx controller |

## Credentials

bazel-remote reads an htpasswd file. Give CI a user that can write, and keep
its password in a repository or organization secret:

```sh
htpasswd -cB htpasswd ci          # prompts for the password
```

The examples start the server with `--allow_unauthenticated_reads`. Anyone
who can reach it can then read the cache, but only the htpasswd users can
write to it. Pull requests from forks get no secrets, so they read
anonymously. Drop the flag to require a credential for reads too.

**Trust.** A cache entry decides what a build links. Only trusted CI should
hold write credentials, for example pushes to `main` and same-repository pull
requests. tinymold re-hashes every object it fetches, so a remote can only
make a build miss, never restore bytes other than the ones the entry names.
The entry itself is trusted as much as whoever wrote it.

## TLS

The credential is sent with every call. Anything that crosses an untrusted
network must use TLS, which means a `grpcs://` URL. Two ways to set it up:

- terminate TLS at an ingress or load balancer that speaks HTTP/2 to the
  backend, as [`k8s/ingress.yaml`](k8s/ingress.yaml) does;
- or give bazel-remote a certificate itself, with `--tls_cert_file` and
  `--tls_key_file`.

## Sizing and expiry

`--max_size` (GiB) is the server's whole budget. Beyond it, bazel-remote
evicts the least recently used blobs, and every hit counts as a use. Entries
that builds keep fetching therefore stay, and entries from code that no
longer exists age out. bazel-remote stores blobs zstd-compressed: a 340 MiB
local tinymold store took 104 MiB on the server.

Give the volume about 10% more room than `--max_size`. A busy monorepo's
debug builds are usually a few GiB per toolchain and lockfile generation.

## Pointing tinymold at it

```sh
export TINYMOLD_REMOTE_URL=grpcs://cache.example.com   # grpc://host:9092 without TLS
export TINYMOLD_REMOTE_AUTH=ci:password                # omit for anonymous reads
tinymold remote check                                  # reachable? credential accepted?
RUSTC_WRAPPER=tinymold cargo build                     # misses consult the remote
tinymold remote push                                   # upload what this machine built
```

In GitHub Actions, use the action's `remote` backend; see
[`docs/specs/github-actions.md`](../../docs/specs/github-actions.md).
