#!/usr/bin/env python3
"""tinymold benchmark harness.

Six benchmarks, each writing one JSON result file under bench/results/:

  baseline
         Plain cargo, default linker, no tinymold: the numbers every later
         phase has to beat. Per corpus project: a clean build, a no-op
         rebuild, a one-line edit, an mtime-only change to every source file
         (what a branch switch or cache restore does), building the tests, and
         a full build in a second worktree at a different path. It also
         measures target/ size and how many bytes the two worktrees' artifacts
         share, which is what a shared content-addressed store would dedupe.

  link   Build each corpus project once per available linker, then time the
         edit loop: append a comment to one source file and rebuild. That
         recompiles the leaf crate and relinks the binary, which is the cost
         developers pay on every save.

  cas    Time restoring one large artifact into a fresh path: a plain copy
         versus `tinymold cas get` by reflink, hardlink, and copy.

Only the Python standard library is used. Everything the harness downloads or
builds lives under bench/.work/ (git-ignored), and each corpus project builds
into its own checkout's target/ directory.

  cache  Clean builds with and without a compile cache: no cache, an empty
         cache (the dry run that fills it), the warm cache (the cached run),
         and the warm cache from a second worktree at another path. The cache
         is sccache, the reference tinymold has to beat, run on a private port
         and directory so it never touches a developer's own sccache server.

  scenarios
         The full edit-loop matrix: per corpus project and per mode
         (baseline, sccache, tinymold), the cases clean, no-op, comment
         edit, private-body edit, public-signature edit, every file touched,
         second worktree, branch switch A->B->A, and cargo test --no-run.
         Mode tinymold builds with RUSTC_WRAPPER=tinymold, runs `tinymold
         fresh` around every build, and restores `target/` snapshots for the
         worktree and branch-switch cases. Records wall time,
         link time (via a timing linker wrapper), units compiled versus fresh,
         target/ bytes by kind, and cache hits.

  dedup  Builds two worktrees of each project and reports how much of the
         second's target/ is byte-identical to the first's, at file level,
         rlib-member level, and DWARF-section level.

Usage:
  python3 bench/bench.py scenarios [--runs N] [--project NAME] [--modes M,..] [--cases C,..]
                                   [--variants V,..]
  python3 bench/bench.py dedup [--project NAME]
  python3 bench/bench.py cache [--runs N] [--project NAME]
  python3 bench/bench.py baseline [--runs N] [--project NAME]
  python3 bench/bench.py link [--runs N] [--project NAME]
  python3 bench/bench.py cas [--runs N] [--size-mb N]
  python3 bench/report.py      # render results into bench/README.md
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

BENCH = Path(__file__).resolve().parent
ROOT = BENCH.parent
WORK = BENCH / ".work"
RESULTS = BENCH / "results"


def stock_cargo() -> tuple[dict, list[str]]:
    """Environment and `--config` flags that pin cargo to its stock behavior.

    Cargo reads `config.toml` from CARGO_HOME and from every ancestor
    directory, so a developer's tuning (a rustc wrapper such as sccache,
    reduced debuginfo, `jobs`, incremental off) silently leaks into every
    corpus build. A baseline has to be stock cargo, so the settings people
    commonly change are forced back to their defaults here, and the forcing is
    recorded with the result. `--config` outranks every config file, and an
    empty RUSTC_WRAPPER disables any configured wrapper.
    """
    env = dict(os.environ, RUSTC_WRAPPER="", CARGO_INCREMENTAL="1")
    env.pop("RUSTFLAGS", None)
    env.pop("CARGO_TARGET_DIR", None)
    flags = [
        f"build.jobs={os.cpu_count()}",
        "profile.dev.debug=true",
        'profile.dev.package."*".debug=true',
        "profile.test.debug=true",
        'profile.test.package."*".debug=true',
    ]
    config = [arg for flag in flags for arg in ("--config", flag)]
    return env, config


STOCK_ENV, STOCK_CONFIG = stock_cargo()


def cargo(*args: str) -> list[str]:
    """A `cargo` command line pinned to stock settings (see stock_cargo)."""
    return ["cargo", *STOCK_CONFIG, *args]


def run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, **kwargs)


def output(cmd: list[str]) -> str:
    return run(cmd, capture_output=True, text=True).stdout.strip()


def timed(cmd: list[str], **kwargs) -> float:
    start = time.perf_counter()
    run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, **kwargs)
    return time.perf_counter() - start


def summarize(samples: list[float]) -> dict:
    return {
        "median_s": round(statistics.median(samples), 4),
        "min_s": round(min(samples), 4),
        "max_s": round(max(samples), 4),
        "samples_s": [round(s, 4) for s in samples],
    }


def machine() -> dict:
    """Everything that moves a benchmark result, recorded with the result.

    Wild's author showed the filesystem, output reuse, fork mode, and thread
    count alone swing linker comparisons by 40% (docs/research/linkers.md), so
    a number without its machine is not a result.
    """
    return {
        "os": platform.system().lower(),
        "os_release": platform.release(),
        "arch": platform.machine(),
        "cpus": os.cpu_count(),
        "rustc": output(["rustc", "-V"]),
        "python": platform.python_version(),
        "cargo_settings": "stock: no rustc wrapper, incremental on, full debuginfo, "
                          f"jobs={os.cpu_count()}, no RUSTFLAGS beyond the linker's",
    }


def host_triple() -> str:
    for line in output(["rustc", "-vV"]).splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ")
    raise SystemExit("could not read the host triple from rustc -vV")


def linkers() -> dict[str, list[str]]:
    """Linkers available on this machine, as extra rustc flags.

    `default` is whatever rustc picks with no flags. `rust-lld` is the lld that
    ships inside every Rust toolchain, selected through the sysroot's `gcc-ld`
    shims so nothing extra needs installing. mold and wild are ELF-only.
    """
    found: dict[str, list[str]] = {"default": []}
    sysroot = output(["rustc", "--print", "sysroot"])
    gcc_ld = Path(sysroot) / "lib" / "rustlib" / host_triple() / "bin" / "gcc-ld"
    if gcc_ld.is_dir():
        found["rust-lld"] = [f"-Clink-arg=-B{gcc_ld}", "-Clink-arg=-fuse-ld=lld"]
    if platform.system() == "Linux":
        if shutil.which("mold"):
            found["mold"] = ["-Clink-arg=-fuse-ld=mold"]
        wild = shutil.which("wild")
        if wild and shutil.which("clang"):
            found["wild"] = ["-Clinker=clang", f"-Clink-arg=--ld-path={wild}"]
    return found


def checkout(project: dict) -> Path:
    if project["repo"] == "local":
        # tinymold itself: a detached worktree of this repository at `rev`.
        sha = output(["git", "-C", str(ROOT), "rev-parse", "--short", project["rev"]])
        path = WORK / f"{project['name']}-{sha}"
        if not path.is_dir():
            WORK.mkdir(parents=True, exist_ok=True)
            run(["git", "-C", str(ROOT), "worktree", "add", "--quiet", "--detach",
                 str(path), sha])
        project["rev"] = sha
    else:
        path = WORK / f"{project['name']}-{project['rev']}"
        if not path.is_dir():
            WORK.mkdir(parents=True, exist_ok=True)
            run(["git", "-c", "advice.detachedHead=false", "clone", "--quiet", "--depth", "1",
                 "--branch", project["rev"], project["repo"], str(path)])
    if not (path / "Cargo.lock").exists():
        # Libraries often ship without a lockfile. Resolve once, then every
        # build runs --locked against the same graph.
        run(["cargo", "generate-lockfile"], cwd=path, env=STOCK_ENV,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return path


def target_args(project: dict) -> list[str]:
    """The cargo arguments selecting what a corpus project builds."""
    args = ["--package", project["package"]]
    if "bin" in project:
        args += ["--bin", project["bin"]]
    elif "example" in project:
        args += ["--example", project["example"]]
    else:
        args.append("--lib")
    if "features" in project:
        args += ["--features", project["features"]]
    return args


def bench_link(args: argparse.Namespace) -> dict:
    corpus = load_corpus(args.project, needs=("bin", "edit"))
    available = linkers()
    print(f"linkers: {', '.join(available)}", file=sys.stderr)

    results = []
    for project in corpus:
        src = checkout(project)
        edit = src / project["edit"]
        pristine = edit.read_text()
        for linker, flags in available.items():
            # One target dir per linker inside the project's own target/, so
            # linkers never invalidate each other's artifacts.
            target = src / "target" / f"bench-{linker}"
            env = dict(STOCK_ENV, RUSTFLAGS=" ".join(flags))
            build = cargo("build", "--quiet", "--locked", "--package", project["package"],
                          "--bin", project["bin"], "--target-dir", str(target))
            print(f"{project['name']} / {linker}: full build", file=sys.stderr)
            shutil.rmtree(target, ignore_errors=True)
            full = timed(build, cwd=src, env=env)
            samples = []
            try:
                for i in range(args.runs):
                    edit.write_text(f"{pristine}\n// tinymold-bench edit {linker} {i}\n")
                    samples.append(timed(build, cwd=src, env=env))
            finally:
                edit.write_text(pristine)
            binary = target / "debug" / (project["bin"] + (".exe" if os.name == "nt" else ""))
            results.append({
                "project": project["name"],
                "rev": project["rev"],
                "linker": linker,
                "rustflags": flags,
                "full_build_s": round(full, 3),
                "edit_rebuild": summarize(samples),
                "binary_bytes": binary.stat().st_size if binary.exists() else None,
                "target_bytes": tree_size(target),
            })
            print(f"  edit-rebuild median {results[-1]['edit_rebuild']['median_s']}s",
                  file=sys.stderr)
    return {"benchmark": "link", "results": results}


def bench_baseline(args: argparse.Namespace) -> dict:
    corpus = load_corpus(args.project, needs=("bin", "edit"))
    results = []
    for project in corpus:
        src = checkout(project)
        build = cargo("build", "--quiet", "--locked", "--package", project["package"],
                      "--bin", project["bin"])
        env = STOCK_ENV
        edit = src / project["edit"]
        pristine = edit.read_text()
        name = project["name"]
        row: dict = {"project": name, "rev": project["rev"]}

        print(f"{name}: clean build", file=sys.stderr)
        # Download once up front: a clean build should measure compiling,
        # not the network.
        run(["cargo", "fetch", "--locked"], cwd=src, env=env, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL)
        shutil.rmtree(src / "target", ignore_errors=True)
        row["clean_build_s"] = round(timed(build, cwd=src, env=env), 3)
        row["noop_rebuild"] = summarize([timed(build, cwd=src, env=env) for _ in range(args.runs)])

        print(f"{name}: edit loop", file=sys.stderr)
        samples = []
        try:
            for i in range(args.runs):
                edit.write_text(f"{pristine}\n// tinymold-bench baseline edit {i}\n")
                samples.append(timed(build, cwd=src, env=env))
        finally:
            edit.write_text(pristine)
        # Restoring the file is itself an edit; settle it before the next step.
        timed(build, cwd=src, env=env)
        row["edit_rebuild"] = summarize(samples)

        print(f"{name}: mtime-only change to every source file", file=sys.stderr)
        sources = [p for p in src.rglob("*.rs") if "target" not in p.relative_to(src).parts]
        samples = []
        for _ in range(args.runs):
            now = time.time()
            for path in sources:
                os.utime(path, (now, now))
            samples.append(timed(build, cwd=src, env=env))
        row["touch_all_rebuild"] = summarize(samples)

        print(f"{name}: build tests", file=sys.stderr)
        tests = cargo("build", "--quiet", "--locked", "--tests", "--package", project["package"])
        row["build_tests_s"] = round(timed(tests, cwd=src, env=env), 3)
        row["target_bytes"] = tree_size(src / "target")

        print(f"{name}: second worktree at another path", file=sys.stderr)
        # A real `git worktree`: same commit, different absolute path, empty
        # target/. Everything it builds, the first worktree already built.
        second = WORK / f"{name}-{project['rev']}-worktree2"
        if second.exists():
            run(["git", "-C", str(src), "worktree", "remove", "--force", str(second)])
        run(["git", "-C", str(src), "worktree", "add", "--quiet", "--detach", str(second)])
        row["second_worktree_build_s"] = round(timed(build, cwd=second, env=env), 3)
        shared, total = shared_artifact_bytes(src / "target" / "debug" / "deps",
                                              second / "target" / "debug" / "deps")
        row["second_worktree_deps_bytes"] = total
        row["second_worktree_identical_deps_bytes"] = shared
        run(["git", "-C", str(src), "worktree", "remove", "--force", str(second)])

        results.append(row)
        print(f"  clean {row['clean_build_s']}s, edit {row['edit_rebuild']['median_s']}s, "
              f"touch-all {row['touch_all_rebuild']['median_s']}s, "
              f"2nd worktree {row['second_worktree_build_s']}s", file=sys.stderr)
    return {"benchmark": "baseline", "results": results}


SCCACHE_PORT = "4227"


def sccache_stats(sccache: str, env: dict) -> dict:
    """Rust compile hits and misses since the last `--zero-stats`."""
    text = output_env([sccache, "--show-stats", "--stats-format", "json"], env)
    stats = json.loads(text).get("stats", {})

    def count(key: str) -> int:
        value = stats.get(key, {})
        return sum(value.get("counts", {}).values()) if isinstance(value, dict) else 0

    return {"hits": count("cache_hits"), "misses": count("cache_misses"),
            "non_cacheable": stats.get("non_cacheable_compilations", 0)}


def tinymold_stats(log: Path) -> dict:
    """rustc invocations the tinymold wrapper served from its cache (hits) or
    compiled (misses), from the per-invocation lines of TINYMOLD_LOG."""
    lines = log.read_text().splitlines() if log.exists() else []
    return {"hits": sum(line.startswith("hit ") for line in lines),
            "misses": sum(line.startswith("miss ") for line in lines)}


def output_env(cmd: list[str], env: dict) -> str:
    return run(cmd, capture_output=True, text=True, env=env).stdout.strip()


def bench_cache(args: argparse.Namespace) -> dict:
    sccache = shutil.which("sccache")
    if sccache is None:
        raise SystemExit("the cache benchmark needs sccache on PATH (cargo install sccache)")
    version = output([sccache, "--version"])
    results = []
    for project in load_corpus(args.project, needs=("bin",)):
        src = checkout(project)
        name = project["name"]
        cache_dir = WORK / "sccache" / name
        build = cargo("build", "--quiet", "--locked", "--package", project["package"],
                      "--bin", project["bin"])
        cached_env = dict(STOCK_ENV, RUSTC_WRAPPER=sccache, SCCACHE_DIR=str(cache_dir),
                          SCCACHE_SERVER_PORT=SCCACHE_PORT, SCCACHE_CACHE_SIZE="50G")
        # sccache refuses to run at all while CARGO_INCREMENTAL=1 is set. Without
        # it, cargo's default dev profile still builds workspace members
        # incrementally, and sccache reports those as non-cacheable.
        cached_env.pop("CARGO_INCREMENTAL")

        def restart_server() -> None:
            subprocess.run([sccache, "--stop-server"], env=cached_env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            run([sccache, "--start-server"], env=cached_env, stdout=subprocess.DEVNULL)

        def clean_build(cwd: Path, env: dict) -> float:
            shutil.rmtree(cwd / "target", ignore_errors=True)
            return timed(build, cwd=cwd, env=env)

        row: dict = {"project": name, "rev": project["rev"]}
        print(f"{name}: clean build, no cache", file=sys.stderr)
        row["no_cache"] = summarize([clean_build(src, STOCK_ENV) for _ in range(args.runs)])

        print(f"{name}: clean build, empty cache (dry run)", file=sys.stderr)
        cold = []
        for _ in range(args.runs):
            shutil.rmtree(cache_dir, ignore_errors=True)
            restart_server()
            cold.append(clean_build(src, cached_env))
        row["cold_cache"] = summarize(cold)
        row["cold_stats"] = sccache_stats(sccache, cached_env)

        print(f"{name}: clean build, warm cache (cached run)", file=sys.stderr)
        warm = []
        for _ in range(args.runs):
            run([sccache, "--zero-stats"], env=cached_env, stdout=subprocess.DEVNULL)
            warm.append(clean_build(src, cached_env))
        row["warm_cache"] = summarize(warm)
        row["warm_stats"] = sccache_stats(sccache, cached_env)

        print(f"{name}: second worktree, warm cache", file=sys.stderr)
        second = WORK / f"{name}-{project['rev']}-worktree2"
        if second.exists():
            run(["git", "-C", str(src), "worktree", "remove", "--force", str(second)])
        run(["git", "-C", str(src), "worktree", "add", "--quiet", "--detach", str(second)])
        samples = []
        try:
            for _ in range(args.runs):
                run([sccache, "--zero-stats"], env=cached_env, stdout=subprocess.DEVNULL)
                samples.append(clean_build(second, cached_env))
            row["second_worktree_warm_cache"] = summarize(samples)
            row["second_worktree_stats"] = sccache_stats(sccache, cached_env)
        finally:
            run(["git", "-C", str(src), "worktree", "remove", "--force", str(second)])
            subprocess.run([sccache, "--stop-server"], env=cached_env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        row["cache_bytes"] = tree_size(cache_dir)

        results.append(row)
        print(f"  no cache {row['no_cache']['median_s']}s, cold {row['cold_cache']['median_s']}s, "
              f"warm {row['warm_cache']['median_s']}s, "
              f"2nd worktree {row['second_worktree_warm_cache']['median_s']}s", file=sys.stderr)
    return {"benchmark": "cache", "cache": version, "results": results}


def shared_artifact_bytes(first: Path, second: Path) -> tuple[int, int]:
    """Bytes of `second`'s artifacts that are byte-identical to one in `first`.

    Matching is by content, not file name: two worktrees at different paths
    produce some artifacts with the same name but different bytes, and that
    gap is exactly what tinymold's path normalization has to close.
    """
    def digests(directory: Path) -> dict[str, int]:
        found: dict[str, int] = {}
        for path in directory.iterdir() if directory.is_dir() else []:
            if path.is_file():
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                found[digest] = path.stat().st_size
        return found

    have = digests(first)
    theirs = digests(second)
    shared = sum(size for digest, size in theirs.items() if digest in have)
    return shared, sum(theirs.values())


def load_corpus(only: str | None, needs: tuple[str, ...] = ()) -> list[dict]:
    """Corpus projects, optionally only `only`, and only those with every key
    in `needs` (the older benchmarks need a binary and a single edit file)."""
    corpus = json.loads((BENCH / "corpus.json").read_text())["projects"]
    return [p for p in corpus
            if (only is None or p["name"] == only) and all(key in p for key in needs)]


def tree_size(path: Path) -> int:
    total = 0
    for root, _dirs, files in os.walk(path):
        for name in files:
            try:
                total += os.lstat(os.path.join(root, name)).st_size
            except OSError:
                pass
    return total


def bench_cas(args: argparse.Namespace) -> dict:
    tinymold = tinymold_binary()

    work = WORK / "cas"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    store = work / "store"
    artifact = work / "artifact.rlib"
    with artifact.open("wb") as f:
        for _ in range(args.size_mb):
            f.write(os.urandom(1024 * 1024))

    put = output([str(tinymold), "--store", str(store), "cas", "put", str(artifact)])
    digest = put.split()[0]

    def copy_once(i: int) -> float:
        dest = work / f"plain-{i}.rlib"
        start = time.perf_counter()
        with artifact.open("rb") as src, dest.open("wb") as dst:
            shutil.copyfileobj(src, dst, 1024 * 1024)
        return time.perf_counter() - start

    methods = {"python byte copy (baseline)": copy_once}
    for strategy in ("reflink", "hardlink", "copy"):
        def once(i: int, strategy: str = strategy) -> float:
            dest = work / f"{strategy}-{i}.rlib"
            return timed([str(tinymold), "--store", str(store), "cas", "get",
                          "--strategy", strategy, digest, str(dest)])
        methods[f"tinymold cas get --strategy {strategy}"] = once

    results = []
    for name, once in methods.items():
        try:
            samples = [once(i) for i in range(args.runs)]
        except subprocess.CalledProcessError:
            print(f"{name}: not supported on this filesystem", file=sys.stderr)
            continue
        results.append({"method": name, "restore": summarize(samples)})
        print(f"{name}: median {results[-1]['restore']['median_s']}s", file=sys.stderr)

    shutil.rmtree(work, ignore_errors=True)
    return {"benchmark": "cas", "artifact_mb": args.size_mb, "results": results}


# ---------------------------------------------------------------------------
# Scenarios: every edit-loop case, per project, per mode
# ---------------------------------------------------------------------------

CASES = ("clean", "noop", "comment", "private_body", "public_signature", "touch_all",
         "second_worktree", "branch_switch", "test_no_run")
MODES = ("baseline", "sccache", "tinymold")

# Edit-loop experiments (roadmap Phase 5): settings layered on the baseline,
# each reported as its own column, `baseline+<name>`. A variant may set cargo
# `--config` values, rustflags for every crate, and a toolchain; nightly-only
# variants are compared with the `nightly` variant, not with stable.
VARIANTS = {
    "deps-nodebug": {"config": ['profile.dev.package."*".debug=false']},
    "line-tables": {"config": ['profile.dev.debug="line-tables-only"',
                               'profile.dev.package."*".debug=false']},
    "nodebug": {"config": ["profile.dev.debug=false",
                           'profile.dev.package."*".debug=false']},
    "cgu-16": {"config": ["profile.dev.codegen-units=16"]},
    "nightly": {"toolchain": "nightly"},
    "threads-8": {"toolchain": "nightly", "rustflags": ["-Zthreads=8"]},
    "cranelift": {"toolchain": "nightly", "rustflags": ["-Zcodegen-backend=cranelift"]},
}

# A linker wrapper that times the real linker and appends the duration to a
# log. Perl because it starts in a few milliseconds on every Unix and its
# Time::HiRes is core; a Python wrapper would add ~30 ms to every link.
LINK_TIMER = """#!/usr/bin/env perl
use strict;
use Time::HiRes qw(time);
my $real = $ENV{TINYMOLD_BENCH_REAL_LINKER} || 'cc';
my $start = time;
my $status = system($real, @ARGV);
my $elapsed = time - $start;
if (my $log = $ENV{TINYMOLD_BENCH_LINK_LOG}) {
    open(my $fh, '>>', $log) or die "cannot append to $log: $!";
    printf $fh "%.6f\\n", $elapsed;
    close($fh);
}
exit($status == -1 ? 127 : $status >> 8);
"""


def link_timer() -> Path:
    path = WORK / "link-timer"
    WORK.mkdir(parents=True, exist_ok=True)
    if not path.exists() or path.read_text() != LINK_TIMER:
        path.write_text(LINK_TIMER)
        path.chmod(0o755)
    return path


def fs_type(path: Path) -> str:
    """The filesystem type holding `path`, e.g. apfs, ext4, btrfs, tmpfs."""
    path.mkdir(parents=True, exist_ok=True)
    if platform.system() == "Linux":
        return output(["stat", "-f", "-c", "%T", str(path)])
    best, kind = "", "unknown"
    for line in output(["/sbin/mount"]).splitlines():
        # "/dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled)"
        if " on " not in line or " (" not in line:
            continue
        mount_point = line.split(" on ", 1)[1].rsplit(" (", 1)[0]
        resolved = str(path.resolve())
        if (resolved == mount_point or resolved.startswith(mount_point.rstrip("/") + "/")) \
                and len(mount_point) > len(best):
            best, kind = mount_point, line.rsplit(" (", 1)[1].split(",")[0]
    return kind


def target_kinds(target: Path) -> dict[str, int]:
    """Bytes in target/ by artifact kind.

    `objects` are loose `.o` files: on macOS, where rustc's default is
    split-debuginfo=unpacked, they are where the debuginfo lives.
    """
    kinds = dict.fromkeys(("rlib", "rmeta", "debuginfo", "objects", "incremental",
                           "executables", "other"), 0)
    for root, dirs, files in os.walk(target):
        parts = Path(root).relative_to(target).parts
        for name in files:
            path = Path(root) / name
            try:
                info = path.lstat()
            except OSError:
                continue
            suffix = path.suffix
            if "incremental" in parts:
                kind = "incremental"
            elif any(p.endswith(".dSYM") for p in parts) or suffix in (".dwo", ".dwp", ".pdb"):
                kind = "debuginfo"
            elif suffix in (".rlib", ".rmeta", ".o"):
                kind = {"rlib": "rlib", "rmeta": "rmeta", "o": "objects"}[suffix[1:]]
            elif suffix in ("", ".exe", ".dylib", ".so", ".dll") and info.st_mode & 0o111:
                kind = "executables"
            else:
                kind = "other"
            kinds[kind] += info.st_size
    return kinds


def unit_counts(stderr: str) -> dict[str, int]:
    """Units cargo compiled versus found fresh, from `cargo build -v` output."""
    lines = [line.strip() for line in stderr.splitlines()]
    return {"compiled": sum(line.startswith("Compiling ") for line in lines),
            "fresh": sum(line.startswith("Fresh ") for line in lines)}


class Edits:
    """Applies one edit kind to a checkout and always restores it afterwards."""

    def __init__(self, src: Path, spec: dict):
        self.src, self.spec, self.saved = src, spec, {}

    def apply(self, kind: str, n: int) -> None:
        edit = self.spec[kind]
        path = self.src / edit["file"]
        text = self.saved.setdefault(path, path.read_text())
        if kind == "comment":
            text = f"{text}\n// tinymold-bench comment edit {n}\n"
        elif kind == "private_body":
            anchor = edit["anchor"]
            if anchor not in text:
                raise SystemExit(f"anchor not found in {edit['file']}: {anchor!r}")
            text = text.replace(anchor, f"{anchor}\n    std::hint::black_box({n});", 1)
        elif kind == "public_signature":
            text = (f"{text}\n/// tinymold benchmark probe; changes the public API.\n"
                    f"#[doc(hidden)]\npub fn __tinymold_bench_probe_{n}() {{}}\n")
        path.write_text(text)

    def restore(self) -> None:
        for path, text in self.saved.items():
            path.write_text(text)
        self.saved.clear()


def tinymold_binary() -> Path:
    """Builds tinymold in release mode and returns the binary's path."""
    run(cargo("build", "--quiet", "--release", "--package", "tinymold"), cwd=ROOT, env=STOCK_ENV)
    return ROOT / "target" / "release" / ("tinymold" + (".exe" if os.name == "nt" else ""))


class Session:
    """One project built in one mode: the commands, env, and measurements.

    Mode `tinymold` wraps every build the way a user's tooling would: `tinymold
    fresh restore` before cargo and `tinymold fresh record` after it, both
    counted in the wall time. The worktree and branch-switch cases also save
    and restore `target/` snapshots (see `run_case` and `branch_switch`).
    """

    def __init__(self, project: dict, src: Path, mode: str, sccache: str | None,
                 tinymold: Path | None = None, variant: str | None = None):
        self.project, self.src, self.mode = project, src, mode
        self.tinymold = tinymold
        self.label = mode if variant is None else f"{mode}+{variant}"
        self.log = WORK / f"link-{project['name']}-{self.label}.log"
        timer = link_timer()
        self.config = ["--config", f"target.{host_triple()}.linker=\"{timer}\""]
        settings = VARIANTS[variant] if variant else {}
        for value in settings.get("config", []):
            self.config += ["--config", value]
        if settings.get("rustflags"):
            flags = ", ".join(json.dumps(flag) for flag in settings["rustflags"])
            self.config += ["--config", f"build.rustflags=[{flags}]"]
        self.toolchain = settings.get("toolchain")
        self.env = dict(STOCK_ENV, TINYMOLD_BENCH_LINK_LOG=str(self.log))
        self.sccache = sccache
        if mode == "sccache":
            self.env.update(RUSTC_WRAPPER=sccache,
                            SCCACHE_DIR=str(WORK / "sccache" / project["name"]),
                            SCCACHE_SERVER_PORT=SCCACHE_PORT, SCCACHE_CACHE_SIZE="50G")
            # sccache refuses to run while CARGO_INCREMENTAL=1 is set.
            self.env.pop("CARGO_INCREMENTAL")
        if mode == "tinymold":
            self.store = WORK / "tinymold-store" / project["name"]
            self.cache_log = WORK / f"tinymold-{project['name']}.log"
            # tinymold caches incremental units too, so CARGO_INCREMENTAL=1
            # stays as in the baseline.
            self.env.update(TINYMOLD_STORE=str(self.store), RUSTC_WRAPPER=str(self.tinymold),
                            TINYMOLD_LOG=str(self.cache_log))

    def tm(self, *args: str, cwd: Path | None = None) -> float:
        """Runs one tinymold command and returns its wall time. `fresh restore`
        exits 1 when it has no usable manifest, which is not a failure here."""
        start = time.perf_counter()
        done = subprocess.run([str(self.tinymold), *args], cwd=cwd or self.src, env=self.env,
                              capture_output=True, text=True)
        if done.returncode not in (0, 1):
            sys.stderr.write(done.stderr)
            raise SystemExit(f"tinymold {' '.join(args)} failed")
        return time.perf_counter() - start

    def build(self, cwd: Path | None = None, tests: bool = False) -> dict:
        """One timed build with link time, unit counts, and cache stats."""
        args = target_args(self.project)
        if tests:
            command = cargo(*self.config, "test", "--no-run", "--locked", "-v",
                            "--package", self.project["package"],
                            *(["--features", self.project["features"]]
                              if "features" in self.project else []))
        else:
            command = cargo(*self.config, "build", "--locked", "-v", *args)
        if self.toolchain:
            command.insert(1, f"+{self.toolchain}")
        self.log.unlink(missing_ok=True)
        if self.mode == "tinymold":
            self.cache_log.unlink(missing_ok=True)
        if self.sccache and self.mode == "sccache":
            run([self.sccache, "--zero-stats"], env=self.env, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL)
        overhead = self.tm("fresh", "restore", cwd=cwd) if self.mode == "tinymold" else 0.0
        start = time.perf_counter()
        done = subprocess.run(command, cwd=cwd or self.src, env=self.env,
                              capture_output=True, text=True)
        wall = time.perf_counter() - start
        if done.returncode != 0:
            sys.stderr.write(done.stderr[-4000:])
            raise SystemExit(f"{self.project['name']}: build failed in mode {self.mode}")
        if self.mode == "tinymold":
            overhead += self.tm("fresh", "record", cwd=cwd)
            wall += overhead
        links = [float(x) for x in self.log.read_text().split()] if self.log.exists() else []
        sample = {"wall": wall, "tinymold": overhead, "link": sum(links), "links": len(links),
                  **unit_counts(done.stderr)}
        if self.mode == "sccache":
            sample["cache"] = sccache_stats(self.sccache, self.env)
        if self.mode == "tinymold":
            sample["cache"] = tinymold_stats(self.cache_log)
        return sample

    def clean(self, cwd: Path | None = None) -> None:
        shutil.rmtree((cwd or self.src) / "target", ignore_errors=True)


def run_case(session: Session, case: str, runs: int) -> dict:
    """Runs one case `runs` times and returns its aggregated measurements."""
    src, project = session.src, session.project
    edits = Edits(src, project["edits"])
    samples: list[dict] = []
    extra: dict = {}
    if case != "clean":
        session.build()  # every other case starts from an up-to-date build
    for n in range(runs):
        if case == "clean":
            session.clean()
            samples.append(session.build())
        elif case == "noop":
            samples.append(session.build())
        elif case in ("comment", "private_body", "public_signature"):
            try:
                edits.apply(case, n)
                samples.append(session.build())
            finally:
                edits.restore()
            session.build()  # settle the restore before the next sample
        elif case == "touch_all":
            now = time.time()
            for path in src.rglob("*.rs"):
                if "target" not in path.relative_to(src).parts:
                    os.utime(path, (now, now))
            samples.append(session.build())
        elif case == "second_worktree":
            second = WORK / f"{project['name']}-{project['rev']}-wt2-{session.mode}"
            if second.exists():
                run(["git", "-C", str(src), "worktree", "remove", "--force", str(second)])
            if session.mode == "tinymold":
                # Saved in the first worktree after its build; not part of the
                # second worktree's wall time, but reported alongside it.
                extra.setdefault("snapshot_save_s", []).append(
                    round(session.tm("snapshot", "save"), 4))
            run(["git", "-C", str(src), "worktree", "add", "--quiet", "--detach", str(second)])
            try:
                restore = (session.tm("snapshot", "restore", cwd=second)
                           if session.mode == "tinymold" else 0.0)
                sample = session.build(cwd=second)
                sample["wall"] += restore
                sample["tinymold"] += restore
                samples.append(sample)
            finally:
                run(["git", "-C", str(src), "worktree", "remove", "--force", str(second)])
        elif case == "branch_switch":
            samples.append(branch_switch(session, edits, n))
        elif case == "test_no_run":
            session.clean()
            session.build()
            samples.append(session.build(tests=True))
            extra["target_kinds"] = target_kinds(src / "target")
    if case == "clean":
        extra["target_kinds"] = target_kinds(src / "target")
        # Under a cache the first clean build fills it (the dry run) and later
        # ones hit it, so the first run is reported on its own.
        extra["first_wall_s"] = round(samples[0]["wall"], 4)

    result = {
        "wall": summarize([s["wall"] for s in samples]),
        "link": summarize([s["link"] for s in samples]),
        "links": statistics.median(s["links"] for s in samples),
        "compiled": statistics.median(s["compiled"] for s in samples),
        "fresh": statistics.median(s["fresh"] for s in samples),
        "tinymold": summarize([s["tinymold"] for s in samples]),
        **extra,
    }
    if session.mode in ("sccache", "tinymold"):
        result["cache"] = {key: sum(s["cache"][key] for s in samples)
                           for key in ("hits", "misses")}
    return result


def branch_switch(session: Session, edits: Edits, n: int) -> dict:
    """A→B→A: switch to a branch with a public API change, build, switch back,
    build. Git rewrites the changed files on each switch, so this measures what
    cargo's mtime-based freshness costs after a round trip that ends exactly
    where it started."""
    src = session.src
    git = ["git", "-C", str(src), "-c", "user.name=tinymold-bench",
           "-c", "user.email=bench@tinymold.invalid"]
    base = output(["git", "-C", str(src), "rev-parse", "HEAD"])
    branch = f"tinymold-bench-b-{n}"
    edits.apply("public_signature", n)
    edits.saved.clear()  # the edit is committed on B, not restored by hand
    run([*git, "switch", "--quiet", "-c", branch])
    run([*git, "commit", "--quiet", "-am", "tinymold bench: branch B"])
    run([*git, "switch", "--quiet", "--detach", base])
    session.build()  # start from a fresh build of A
    # Under tinymold the return to A is served by the rustc wrapper's unit
    # cache. Snapshots are not used here: saving one right after a build waits
    # for the filesystem to flush it, which cost more than it saved on bat
    # (2.42 s against 1.47 s without; see docs/plans/rustc-wrapper.md).
    try:
        run([*git, "switch", "--quiet", branch])
        to_b = session.build()
        run([*git, "switch", "--quiet", "--detach", base])
        back = session.build()
    finally:
        run([*git, "switch", "--quiet", "--detach", base])
        run([*git, "branch", "--quiet", "-D", branch])
    merged = {key: to_b[key] + back[key]
              for key in ("wall", "tinymold", "link", "links", "compiled", "fresh")}
    if "cache" in back:
        merged["cache"] = {key: to_b["cache"][key] + back["cache"][key]
                           for key in ("hits", "misses")}
    return merged


def bench_scenarios(args: argparse.Namespace) -> dict:
    modes = args.modes.split(",")
    for mode in modes:
        if mode not in MODES:
            raise SystemExit(f"unknown mode {mode!r}; choose from {', '.join(MODES)}")
    tinymold = tinymold_binary() if "tinymold" in modes else None
    sccache = shutil.which("sccache")
    if "sccache" in modes and sccache is None:
        raise SystemExit("mode 'sccache' needs sccache on PATH (cargo install sccache)")
    cases = args.cases.split(",") if args.cases else list(CASES)
    variants = args.variants.split(",") if args.variants else []
    for variant in variants:
        if variant not in VARIANTS:
            raise SystemExit(f"unknown variant {variant!r}; choose from {', '.join(VARIANTS)}")
    sessions = [(mode, None) for mode in modes] + [("baseline", v) for v in variants]

    results = []
    for project in load_corpus(args.project, needs=("edits",)):
        src = checkout(project)
        run(["cargo", "fetch", "--locked"], cwd=src, env=STOCK_ENV,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for mode, variant in sessions:
            session = Session(project, src, mode, sccache, tinymold, variant)
            if mode == "tinymold":
                shutil.rmtree(session.store, ignore_errors=True)
            if mode == "sccache":
                subprocess.run([sccache, "--stop-server"], env=session.env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                shutil.rmtree(WORK / "sccache" / project["name"], ignore_errors=True)
                run([sccache, "--start-server"], env=session.env, stdout=subprocess.DEVNULL)
            try:
                for case in cases:
                    print(f"{project['name']} / {session.label} / {case}", file=sys.stderr)
                    outcome = run_case(session, case, args.runs)
                    results.append({"project": project["name"], "rev": project["rev"],
                                    "role": project.get("role", ""), "mode": session.label,
                                    "case": case, **outcome})
                    print(f"  wall {outcome['wall']['median_s']}s, "
                          f"link {outcome['link']['median_s']}s, "
                          f"compiled {outcome['compiled']}", file=sys.stderr)
            finally:
                if mode == "sccache":
                    subprocess.run([sccache, "--stop-server"], env=session.env,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if variants:
        # Experiments get their own result file, so they never crowd the
        # scenario table the roadmap's gates are read from.
        return {"benchmark": "experiments", "pins": pins(), "results": results,
                "variants": {name: VARIANTS[name] for name in variants}}
    return {"benchmark": "scenarios", "pins": pins(), "results": results}


def pins() -> dict:
    """What the scenario numbers depend on, pinned where cargo allows it."""
    return {
        "cargo_build_jobs": os.cpu_count(),
        "target_filesystem": fs_type(WORK),
        "linker": "cc, rustc's default, behind a timing wrapper",
        "linker_threads": "linker default; the platform cc linker takes no thread flag",
        "output_reuse": "rustc default: each link writes a fresh output",
        "incremental": "on for baseline; off under sccache, which refuses it",
    }


# ---------------------------------------------------------------------------
# Dedup probe: how much of two worktrees' target/ is byte-identical
# ---------------------------------------------------------------------------

def ar_members(data: bytes):
    """Yields (name, bytes) for each member of an ar archive, such as an rlib."""
    if not data.startswith(b"!<arch>\n"):
        return
    offset, long_names = 8, b""
    while offset + 60 <= len(data):
        header = data[offset:offset + 60]
        name = header[:16].decode("utf-8", "replace").strip()
        size = int(header[48:58].decode().strip() or 0)
        body = data[offset + 60:offset + 60 + size]
        offset += 60 + size + (size & 1)
        if name.startswith("#1/"):  # BSD: the name precedes the body
            length = int(name[3:])
            name, body = body[:length].rstrip(b"\0").decode("utf-8", "replace"), body[length:]
        elif name == "//":  # GNU long-name table
            long_names = body
            continue
        elif name.startswith("/") and name[1:].isdigit():
            start = int(name[1:])
            name = long_names[start:long_names.index(b"/\n", start)].decode("utf-8", "replace")
        if name in ("/", "/SYM64/", "__.SYMDEF", "__.SYMDEF SORTED"):
            continue
        yield name.rstrip("/"), body


def dwarf_sections(data: bytes):
    """Yields (name, bytes) for each DWARF section in an ELF or Mach-O object."""
    import struct

    if data[:4] == b"\x7fELF" and data[4] == 2:  # ELF64, little-endian
        shoff, = struct.unpack_from("<Q", data, 0x28)
        shentsize, shnum, shstrndx = struct.unpack_from("<HHH", data, 0x3A)
        headers = [struct.unpack_from("<IIQQQQIIQQ", data, shoff + i * shentsize)
                   for i in range(shnum)]
        names_at = headers[shstrndx][4]
        for name_off, kind, _flags, _addr, off, size, *_ in headers:
            end = data.index(b"\0", names_at + name_off)
            name = data[names_at + name_off:end].decode()
            if name.startswith(".debug_") and kind != 8:  # 8 = SHT_NOBITS
                yield name, data[off:off + size]
    elif data[:4] == b"\xcf\xfa\xed\xfe":  # Mach-O 64, little-endian
        ncmds, = struct.unpack_from("<I", data, 16)
        offset = 32
        for _ in range(ncmds):
            cmd, cmdsize = struct.unpack_from("<II", data, offset)
            if cmd == 0x19:  # LC_SEGMENT_64
                nsects, = struct.unpack_from("<I", data, offset + 64)
                for i in range(nsects):
                    sect = offset + 72 + i * 80
                    name = data[sect:sect + 16].rstrip(b"\0").decode()
                    segment = data[sect + 16:sect + 32].rstrip(b"\0").decode()
                    size, = struct.unpack_from("<Q", data, sect + 40)
                    off, = struct.unpack_from("<I", data, sect + 48)
                    if segment == "__DWARF" and off:
                        yield name, data[off:off + size]
            offset += cmdsize


def inventory(target: Path) -> dict[str, dict[str, int]]:
    """Content digests and sizes at three granularities for one target/."""
    files: dict[str, int] = {}
    members: dict[str, int] = {}
    dwarf: dict[str, int] = {}

    def objects(name: str, data: bytes) -> None:
        if name.endswith(".o"):
            for _, section in dwarf_sections(data):
                if section:
                    dwarf[hashlib.sha256(section).hexdigest()] = len(section)

    for root, _dirs, names in os.walk(target):
        if "incremental" in Path(root).parts:
            continue
        for name in names:
            path = Path(root) / name
            if path.is_symlink() or not path.is_file():
                continue
            data = path.read_bytes()
            files[hashlib.sha256(data).hexdigest()] = len(data)
            if name.endswith(".rlib"):
                for member, body in ar_members(data):
                    members[hashlib.sha256(body).hexdigest()] = len(body)
                    objects(member, body)
            else:
                objects(name, data)
    return {"files": files, "rlib_members": members, "dwarf_sections": dwarf}


def bench_dedup(args: argparse.Namespace) -> dict:
    results = []
    for project in load_corpus(args.project, needs=("edits",)):
        src = checkout(project)
        session = Session(project, src, "baseline", None)
        print(f"{project['name']}: building two worktrees", file=sys.stderr)
        session.build()
        second = WORK / f"{project['name']}-{project['rev']}-dedup"
        if second.exists():
            run(["git", "-C", str(src), "worktree", "remove", "--force", str(second)])
        run(["git", "-C", str(src), "worktree", "add", "--quiet", "--detach", str(second)])
        try:
            session.build(cwd=second)
            first, other = inventory(src / "target"), inventory(second / "target")
        finally:
            run(["git", "-C", str(src), "worktree", "remove", "--force", str(second)])
        row = {"project": project["name"], "rev": project["rev"]}
        for level in ("files", "rlib_members", "dwarf_sections"):
            total = sum(other[level].values())
            shared = sum(size for digest, size in other[level].items() if digest in first[level])
            row[level] = {"bytes": total, "identical_bytes": shared,
                          "unique_items": len(other[level])}
        results.append(row)
        print(f"  identical: files {pct(row['files'])}, rlib members "
              f"{pct(row['rlib_members'])}, DWARF {pct(row['dwarf_sections'])}", file=sys.stderr)
    return {"benchmark": "dedup", "results": results}


def pct(level: dict) -> str:
    return f"{100 * level['identical_bytes'] / max(1, level['bytes']):.0f}%"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="benchmark", required=True)
    scenarios = sub.add_parser("scenarios", help="every edit-loop case, per project and mode")
    scenarios.add_argument("--runs", type=int, default=3)
    scenarios.add_argument("--project", help="only this corpus project")
    scenarios.add_argument("--modes", default="baseline,sccache",
                           help=f"comma-separated, from: {', '.join(MODES)}")
    scenarios.add_argument("--cases", help=f"comma-separated subset of: {', '.join(CASES)}")
    scenarios.add_argument("--variants", help="edit-loop experiments on top of the baseline, "
                           f"comma-separated, from: {', '.join(VARIANTS)}")
    dedup = sub.add_parser("dedup", help="byte-identical share of two worktrees' target/")
    dedup.add_argument("--project", help="only this corpus project")
    cache = sub.add_parser("cache", help="clean builds: no cache, cold cache, warm cache")
    cache.add_argument("--runs", type=int, default=3)
    cache.add_argument("--project", help="only this corpus project")
    baseline = sub.add_parser("baseline", help="plain cargo, the numbers to beat")
    baseline.add_argument("--runs", type=int, default=5)
    baseline.add_argument("--project", help="only this corpus project")
    link = sub.add_parser("link", help="edit-rebuild loop per linker")
    link.add_argument("--runs", type=int, default=5)
    link.add_argument("--project", help="only this corpus project")
    cas = sub.add_parser("cas", help="artifact restore by strategy")
    cas.add_argument("--runs", type=int, default=5)
    cas.add_argument("--size-mb", type=int, default=512)
    args = parser.parse_args()

    benchmarks = {"scenarios": bench_scenarios, "dedup": bench_dedup, "cache": bench_cache, "baseline": bench_baseline, "link": bench_link, "cas": bench_cas}
    result = benchmarks[args.benchmark](args)
    result["machine"] = machine()
    result["date"] = dt.date.today().isoformat()
    result["runs"] = getattr(args, "runs", 1)

    RESULTS.mkdir(exist_ok=True)
    name = f"{result['benchmark']}-{result['machine']['os']}-{result['machine']['arch']}.json"
    path = RESULTS / name
    if getattr(args, "project", None) and path.exists():
        # A run limited to one project replaces exactly the rows it measured,
        # by project and, for scenarios, mode and case; the rest are kept.
        previous = json.loads(path.read_text())
        measured = {(r["project"], r.get("mode"), r.get("case")) for r in result["results"]}
        kept = [r for r in previous["results"]
                if (r["project"], r.get("mode"), r.get("case")) not in measured]
        result["results"] = kept + result["results"]
    path.write_text(json.dumps(result, indent=2) + "\n")
    print(f"wrote bench/results/{name}", file=sys.stderr)


if __name__ == "__main__":
    main()
