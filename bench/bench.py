#!/usr/bin/env python3
"""tinymold benchmark harness.

Three benchmarks, each writing one JSON result file under bench/results/:

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

Usage:
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
    path = WORK / f"{project['name']}-{project['rev']}"
    if not path.is_dir():
        WORK.mkdir(parents=True, exist_ok=True)
        run(["git", "-c", "advice.detachedHead=false", "clone", "--quiet", "--depth", "1", "--branch", project["rev"],
             project["repo"], str(path)])
    return path


def bench_link(args: argparse.Namespace) -> dict:
    corpus = load_corpus(args.project)
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
    corpus = load_corpus(args.project)
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


def output_env(cmd: list[str], env: dict) -> str:
    return run(cmd, capture_output=True, text=True, env=env).stdout.strip()


def bench_cache(args: argparse.Namespace) -> dict:
    sccache = shutil.which("sccache")
    if sccache is None:
        raise SystemExit("the cache benchmark needs sccache on PATH (cargo install sccache)")
    version = output([sccache, "--version"])
    results = []
    for project in load_corpus(args.project):
        src = checkout(project)
        name = project["name"]
        cache_dir = WORK / "sccache" / name
        build = cargo("build", "--quiet", "--locked", "--package", project["package"],
                      "--bin", project["bin"])
        cached_env = dict(STOCK_ENV, RUSTC_WRAPPER=sccache, SCCACHE_DIR=str(cache_dir),
                          SCCACHE_SERVER_PORT=SCCACHE_PORT, SCCACHE_CACHE_SIZE="50G")

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


def load_corpus(only: str | None) -> list[dict]:
    corpus = json.loads((BENCH / "corpus.json").read_text())["projects"]
    return [p for p in corpus if only is None or p["name"] == only]


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
    run(cargo("build", "--quiet", "--release", "--package", "tinymold"), cwd=ROOT, env=STOCK_ENV)
    tinymold = ROOT / "target" / "release" / ("tinymold" + (".exe" if os.name == "nt" else ""))

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


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="benchmark", required=True)
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

    benchmarks = {"cache": bench_cache, "baseline": bench_baseline, "link": bench_link, "cas": bench_cas}
    result = benchmarks[args.benchmark](args)
    result["machine"] = machine()
    result["date"] = dt.date.today().isoformat()
    result["runs"] = args.runs

    RESULTS.mkdir(exist_ok=True)
    name = f"{result['benchmark']}-{result['machine']['os']}-{result['machine']['arch']}.json"
    (RESULTS / name).write_text(json.dumps(result, indent=2) + "\n")
    print(f"wrote bench/results/{name}", file=sys.stderr)


if __name__ == "__main__":
    main()
