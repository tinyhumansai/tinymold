"""The edit-sequence benchmark: what a day of edits costs, step by step.

Every other edit benchmark reverts its edit after each build, so it never
sees what builds up. This one applies N edits cumulatively to one checkout
and, after every edit, runs the loop that developers, CI and agents repeat:

  build     cargo build of the project's binary or library
  clippy    cargo clippy --all-targets
  test      cargo test --no-run (the build and links), then cargo test (the run)
  coverage  cargo llvm-cov --summary-only (an instrumented build plus a run)

Each command is timed on its own, with its link time, the units it compiled,
the largest process's peak RSS and, under tinymold, cache hits. After every
step the session's target directory and store are sized three ways: apparent
bytes, allocated blocks (each inode counted once), and the change in the
volume's free space, the only one that sees blocks shared by reflinks.

Every `--churn-every` steps, a churn step flips the project's feature set
(corpus key `churn`), so artifacts of an older configuration are left behind
the way a feature or dependency change leaves them in real use. Every
`--fresh-every` steps, a fresh step deletes the target directory but keeps the
store: a new worktree, a `cargo clean` or a CI runner. Under tinymold that
build is served from the store, so the edit after it shows what a cache hit
leaves for rustc's incremental state.

All sessions see the same edits: each step edits the checkout once and then
runs every session's commands, each session in its own target directory
under the checkout's target/, so load drift hits every side equally.

Cargo settings are pinned through a `.cargo/config.toml` in bench/.work,
because cargo-llvm-cov takes no `--config`; every checkout lives below it, so
it outranks the developer's own CARGO_HOME config.
"""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import bench

COMMANDS = ("build", "clippy", "test", "coverage")
EDIT_MODES = ("baseline", "sccache", "tinymold")
KINDS = ("comment", "private_body", "public_signature")

# Stock settings for every cargo call below bench/.work (see stock_cargo in
# bench.py). The environment still sets RUSTC_WRAPPER and CARGO_INCREMENTAL,
# which outrank any config file.
STOCK_CONFIG_TOML = """# Written by bench/edits.py: pins stock cargo settings for every corpus
# build, including cargo-llvm-cov's, which take no --config.
[build]
jobs = {jobs}

[profile.dev]
debug = true

[profile.dev.package."*"]
debug = true

[profile.test]
debug = true

[profile.test.package."*"]
debug = true
"""


def pin_cargo_config() -> Path:
    path = bench.WORK / ".cargo" / "config.toml"
    path.parent.mkdir(parents=True, exist_ok=True)
    text = STOCK_CONFIG_TOML.format(jobs=os.cpu_count())
    if not path.exists() or path.read_text() != text:
        path.write_text(text)
    return path


def schedule(steps: int, churn_every: int, has_churn: bool, fresh_every: int = 0) -> list[str]:
    """The edit applied at each step: the three edit kinds in turn, with a
    churn step every `churn_every` steps when the project defines one, and a
    fresh step every `fresh_every` steps, which wins when both fall due."""
    plan: list[str] = []
    for step in range(1, steps + 1):
        if fresh_every and step % fresh_every == 0:
            plan.append("fresh")
        elif has_churn and churn_every and step % churn_every == 0:
            plan.append("churn")
        else:
            plan.append(KINDS[sum(kind in KINDS for kind in plan) % len(KINDS)])
    return plan


def rss_bytes(maxrss: int, system: str | None = None) -> int:
    """ru_maxrss in bytes: Linux reports kilobytes, macOS bytes."""
    return maxrss if (system or platform.system()) == "Darwin" else maxrss * 1024


def disk_usage(path: Path) -> dict[str, int]:
    """Apparent and allocated bytes under `path`, each inode counted once, so
    hardlinks count once and reflinked files count in full (no portable API
    tells which blocks a clone shares)."""
    seen: set[tuple[int, int]] = set()
    apparent = allocated = files = 0
    for root, _dirs, names in os.walk(path):
        for name in names:
            try:
                info = os.lstat(os.path.join(root, name))
            except OSError:
                continue
            if (info.st_dev, info.st_ino) in seen:
                continue
            seen.add((info.st_dev, info.st_ino))
            files += 1
            apparent += info.st_size
            allocated += getattr(info, "st_blocks", 0) * 512
    return {"apparent": apparent, "allocated": allocated, "files": files}


def free_bytes(path: Path) -> int:
    stat = os.statvfs(path)
    return stat.f_bavail * stat.f_frsize


def measure(cmd: list[str], cwd: Path, env: dict) -> dict:
    """Runs `cmd` and returns its wall time, exit code, stderr and the peak
    RSS of its largest process. wait4 reports the child's usage including
    every descendant it reaped, so ru_maxrss is the biggest rustc, linker or
    wrapper process of the whole cargo run."""
    with tempfile.TemporaryFile() as err:
        start = time.perf_counter()
        proc = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=err)
        _pid, status, usage = os.wait4(proc.pid, 0)
        wall = time.perf_counter() - start
        proc.returncode = os.waitstatus_to_exitcode(status)
        err.seek(0)
        stderr = err.read().decode(errors="replace")
    return {"wall": wall, "rc": proc.returncode, "stderr": stderr,
            "rss": rss_bytes(usage.ru_maxrss)}


def unit_counts(stderr: str) -> dict[str, int]:
    """Units compiled (or checked, under clippy) versus fresh, from `-v`."""
    lines = [line.strip() for line in stderr.splitlines()]
    return {"compiled": sum(line.startswith(("Compiling ", "Checking ")) for line in lines),
            "fresh": sum(line.startswith("Fresh ") for line in lines)}


def target_split(target: Path) -> dict[str, dict[str, int]]:
    """The target directory sized by part: incremental session dirs, the
    coverage build, and everything else."""
    coverage = target / "llvm-cov-target"
    incremental = [p for p in (target / "debug" / "incremental",
                               coverage / "debug" / "incremental") if p.is_dir()]
    total = disk_usage(target)
    parts = {"total": total, "coverage": disk_usage(coverage),
             "incremental": {"apparent": 0, "allocated": 0, "files": 0}}
    for path in incremental:
        for key, value in disk_usage(path).items():
            parts["incremental"][key] += value
    return parts


class EditSession:
    """One project in one mode with one linker: its env, target dir, store."""

    def __init__(self, project: dict, src: Path, mode: str, linker: str,
                 linker_flags: list[str], tinymold: Path | None, sccache: str | None,
                 name: str | None = None):
        self.project, self.src, self.mode, self.linker = project, src, mode, linker
        self.name = name = name or mode
        self.label = name if linker == "default" else f"{name}+{linker}"
        self.target = src / "target" / f"edits-{self.label}"
        self.log = bench.WORK / f"link-edits-{project['name']}-{self.label}.log"
        triple = bench.host_triple().upper().replace("-", "_")
        self.env = dict(bench.STOCK_ENV,
                        CARGO_BUILD_TARGET_DIR=str(self.target),
                        RUSTFLAGS=" ".join(linker_flags),
                        TINYMOLD_BENCH_LINK_LOG=str(self.log),
                        **{f"CARGO_TARGET_{triple}_LINKER": str(bench.link_timer())})
        self.store: Path | None = None
        if mode == "tinymold":
            self.store = bench.WORK / "tinymold-store" / f"edits-{project['name']}-{self.label}"
            self.cache_log = bench.WORK / f"tinymold-edits-{project['name']}-{self.label}.log"
            # Collections run between steps when asked (--gc), never in the
            # background of a timed build.
            self.env.update(TINYMOLD_STORE=str(self.store), RUSTC_WRAPPER=str(tinymold),
                            TINYMOLD_LOG=str(self.cache_log), TINYMOLD_AUTO_GC="0")
            self.tinymold = tinymold
        if mode == "sccache":
            self.store = bench.WORK / "sccache" / f"edits-{project['name']}-{self.label}"
            self.env.update(RUSTC_WRAPPER=sccache, SCCACHE_DIR=str(self.store),
                            SCCACHE_SERVER_PORT=bench.SCCACHE_PORT, SCCACHE_CACHE_SIZE="50G")
            self.env.pop("CARGO_INCREMENTAL")
        self.churned = False

    def reset(self) -> None:
        shutil.rmtree(self.target, ignore_errors=True)
        if self.store:
            shutil.rmtree(self.store, ignore_errors=True)

    def package_args(self) -> list[str]:
        args = ["--package", self.project["package"]]
        features = [self.project["features"]] if "features" in self.project else []
        if features:
            args += ["--features", ",".join(features)]
        if self.churned:
            args += self.project["churn"]
        return args

    def commands(self, name: str) -> list[tuple[str, list[str], bool]]:
        """(label, command line, whether a non-zero exit is tolerated)."""
        base = ["--locked", "-v", *self.package_args()]
        if name == "build":
            target = bench.target_args(self.project)[2:]  # past "--package <name>"
            return [("build", ["cargo", "build", *base, *target], False)]
        if name == "clippy":
            # Lints are capped at warnings: a corpus release that a newer
            # clippy denies is a lint result, not a failed measurement, and a
            # denied crate would stop cargo before the rest are linted.
            return [("clippy", ["cargo", "clippy", *base, "--all-targets", "--",
                                "--cap-lints=warn"], False)]
        if name == "test":
            # Corpus test suites are run, not judged: a shallow clone without
            # submodules or fixtures fails some tests, which is not a result.
            return [("test_build", ["cargo", "test", *base, "--no-run"], False),
                    ("test_run", ["cargo", "test", *base, "--no-fail-fast"], True)]
        if name == "coverage":
            return [("coverage", ["cargo", "llvm-cov", "--locked", "-v", *self.package_args(),
                                  "--summary-only", "--ignore-run-fail"], True)]
        raise SystemExit(f"unknown command {name!r}; choose from {', '.join(COMMANDS)}")

    def run(self, name: str) -> dict:
        rows = {}
        for label, cmd, tolerated in self.commands(name):
            self.log.unlink(missing_ok=True)
            if self.mode == "tinymold":
                self.cache_log.unlink(missing_ok=True)
            done = measure(cmd, self.src, self.env)
            if done["rc"] != 0 and not tolerated:
                # rustc's own error lines first: the tail is mostly the
                # failed command line.
                errors = [line for line in done["stderr"].splitlines()
                          if line.lstrip().startswith(("error", "-->"))]
                sys.stderr.write("\n".join(errors[:40]) + "\n" + done["stderr"][-2000:])
                raise SystemExit(f"{self.project['name']}: {label} failed in {self.label}")
            links = ([float(x) for x in self.log.read_text().split()]
                     if self.log.exists() else [])
            row = {"wall_s": round(done["wall"], 4), "rc": done["rc"],
                   "rss_mb": round(done["rss"] / 2**20, 1),
                   "link_s": round(sum(links), 4), "links": len(links),
                   **unit_counts(done["stderr"])}
            if self.mode == "tinymold":
                row["cache"] = bench.tinymold_stats(self.cache_log)
            rows[label] = row
        return rows

    def collect(self, keep: int) -> dict:
        """Runs `tinymold gc --keep-generations keep` with no grace period,
        as the background collection converges to, and returns its time."""
        start = time.perf_counter()
        bench.run([str(self.tinymold), "--store", str(self.store), "gc",
                   "--keep-generations", str(keep), "--grace", "0s"],
                  env=self.env, stdout=subprocess.DEVNULL)
        return {"gc_s": round(time.perf_counter() - start, 4)}

    def sizes(self) -> dict:
        sizes = {"target": target_split(self.target)}
        if self.store:
            sizes["store"] = disk_usage(self.store)
        return sizes


def apply_edit(edits: bench.Edits, kind: str, step: int, sessions: list[EditSession]) -> None:
    if kind == "churn":
        for session in sessions:
            session.churned = not session.churned
    elif kind == "fresh":
        for session in sessions:
            shutil.rmtree(session.target, ignore_errors=True)
    else:
        edits.apply(kind, step)


def run_step(session: EditSession, commands: list[str], keep: int | None = None) -> dict:
    free_before = free_bytes(session.src)
    row = {"commands": {}}
    for name in commands:
        row["commands"].update(session.run(name))
    row["free_delta"] = free_before - free_bytes(session.src)
    row.update(session.sizes())
    # Only the build under test collects: an --against binary may predate
    # --keep-generations.
    if keep is not None and session.name == "tinymold":
        row.update(session.collect(keep))
        row["store_after_gc"] = disk_usage(session.store)
    return row


def bench_edits(args: argparse.Namespace) -> dict:
    modes = args.modes.split(",")
    for mode in modes:
        if mode not in EDIT_MODES:
            raise SystemExit(f"unknown mode {mode!r}; choose from {', '.join(EDIT_MODES)}")
    commands = args.commands.split(",")
    for name in commands:
        if name not in COMMANDS:
            raise SystemExit(f"unknown command {name!r}; choose from {', '.join(COMMANDS)}")
    if "coverage" in commands and shutil.which("cargo-llvm-cov") is None:
        raise SystemExit("command 'coverage' needs cargo-llvm-cov (cargo install cargo-llvm-cov)")
    available = bench.linkers()
    linkers = args.linkers.split(",")
    for linker in linkers:
        if linker not in available:
            raise SystemExit(f"linker {linker!r} is not available here; found: "
                             f"{', '.join(available)}")
    sccache = shutil.which("sccache")
    if "sccache" in modes and sccache is None:
        raise SystemExit("mode 'sccache' needs sccache on PATH (cargo install sccache)")
    tinymold = bench.tinymold_binary() if "tinymold" in modes else None
    pin_cargo_config()

    results = []
    for project in bench.load_corpus(args.project, needs=("edits",)):
        src = bench.checkout(project)
        bench.run(["cargo", "fetch", "--locked"], cwd=src, env=bench.STOCK_ENV,
                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        sessions = [EditSession(project, src, mode, linker, available[linker], tinymold, sccache)
                    for linker in linkers for mode in modes]
        if args.against:
            # Another build of tinymold on the same edits, with its own store:
            # the before side of a before-and-after comparison.
            sessions += [EditSession(project, src, "tinymold", linker, available[linker],
                                     Path(args.against).resolve(), sccache, "tinymold-base")
                         for linker in linkers]
        rows: dict[str, list[dict]] = {s.label: [] for s in sessions}
        edited = sorted({edit["file"] for edit in project["edits"].values()})
        dirty = bench.output(["git", "-C", str(src), "status", "--porcelain", "--", *edited])
        if dirty:
            # Edits left by an interrupted run would make every later edit a
            # different one, or a duplicate definition.
            raise SystemExit(f"{project['name']}: edited files are not clean in {src}:\n{dirty}")
        plan = schedule(args.steps, args.churn_every, "churn" in project, args.fresh_every)
        edits = bench.Edits(src, project["edits"], cumulative=True)
        servers = [s for s in sessions if s.mode == "sccache"]
        try:
            for session in servers:
                bench.run([sccache, "--start-server"], env=session.env,
                          stdout=subprocess.DEVNULL)
            for session in sessions:
                session.reset()
                print(f"{project['name']} / {session.label} / warm-up", file=sys.stderr)
                rows[session.label].append({"step": 0, "edit": "clean",
                                            "load": round(os.getloadavg()[0], 2),
                                            **run_step(session, commands, args.gc)})
            for step, kind in enumerate(plan, start=1):
                apply_edit(edits, kind, step, sessions)
                for session in sessions:
                    row = {"step": step, "edit": kind, "load": round(os.getloadavg()[0], 2),
                           **run_step(session, commands, args.gc)}
                    rows[session.label].append(row)
                    walls = ", ".join(f"{k} {v['wall_s']}s" for k, v in row["commands"].items())
                    print(f"{project['name']} / {session.label} / {step} {kind}: {walls}",
                          file=sys.stderr)
        finally:
            edits.restore()
            for session in servers:
                subprocess.run([sccache, "--stop-server"], env=session.env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for session in sessions:
            results.append({"project": project["name"], "rev": project["rev"],
                            "mode": session.label, "linker": session.linker,
                            "steps": rows[session.label]})
            session.reset()
    return {"benchmark": "edits", "pins": {**bench.pins(), "linker": "per row"},
            "commands": commands, "schedule": "comment, private_body, public_signature in turn; "
            f"churn every {args.churn_every} steps; fresh target every {args.fresh_every} steps"
            + (f"; gc --keep-generations {args.gc} after each step" if args.gc is not None else ""),
            "results": results}


def add_parser(sub) -> None:
    edits = sub.add_parser("edits", help="a cumulative sequence of edits: build, clippy, "
                           "tests and coverage after each, with disk and RAM")
    edits.add_argument("--steps", type=int, default=30)
    edits.add_argument("--churn-every", type=int, default=10)
    edits.add_argument("--fresh-every", type=int, default=15,
                       help="delete the target dir every N steps, keeping the store")
    edits.add_argument("--project", help="only this corpus project")
    edits.add_argument("--modes", default="baseline,tinymold",
                       help=f"comma-separated, from: {', '.join(EDIT_MODES)}")
    edits.add_argument("--commands", default=",".join(COMMANDS),
                       help=f"comma-separated, from: {', '.join(COMMANDS)}")
    edits.add_argument("--gc", type=int, metavar="KEEP",
                       help="after every step, run tinymold gc --keep-generations KEEP "
                       "with no grace period (outside the timings)")
    edits.add_argument("--against", metavar="BINARY",
                       help="another tinymold binary, run as mode tinymold-base")
    edits.add_argument("--linkers", default="default",
                       help="comma-separated: default, rust-lld, and on Linux mold or wild")
