"""The per-command cache benchmark: what a warm cache saves each command.

Developers, CI jobs and agents run the same four cargo commands over and
over, and each builds its own flavor of the dependency graph:

  build     cargo build of the project's binary or library
  clippy    cargo clippy --all-targets (clippy-driver for workspace crates)
  test      cargo test --no-run (the build and links), then cargo test (the run)
  coverage  cargo llvm-cov --summary-only (an instrumented build plus a run)

For every corpus project, command and mode, each run measures two cases:

  cold   an empty target directory and an empty cache: the first build ever,
         which under a cache also fills it
  warm   the target directory deleted, the cache kept: a CI runner that
         restored its cache, a new worktree's target, or a `cargo clean`

Plain cargo has no cache, so its warm case is a second cold build and shows
the noise between the two. Each command starts from an empty target and an
empty cache of its own, so no command is served by another's work, and the
modes take turns inside every run so load drift hits every side equally.

The commands, the stock cargo settings and the per-command measurements
(wall time, link time, units compiled, peak RSS, cache hits) are the edit
sequence's (bench/edits.py), so the two benchmarks read the same way.
"""

from __future__ import annotations

import argparse
import shutil
import statistics
import subprocess
import sys

import bench
import edits

MODES = ("baseline", "sccache", "tinymold")
CASES = ("cold", "warm")
DEFAULT_PROJECTS = ("ripgrep", "bat", "tinymold")


def aggregate(samples: list[dict]) -> dict:
    """Folds one case's runs into a summary per command label.

    Each sample is what `EditSession.run` returns for one command: a row per
    label (`test` gives `test_build` and `test_run`). Wall and link times are
    summarized across runs; units compiled is the median; cache hits and
    misses are summed, so a hit rate reads across every run.
    """
    labels = dict.fromkeys(label for sample in samples for label in sample)
    summary = {}
    for label in labels:
        rows = [sample[label] for sample in samples if label in sample]
        entry = {"wall": bench.summarize([row["wall_s"] for row in rows]),
                 "link_s": round(statistics.median(row["link_s"] for row in rows), 4),
                 "compiled": statistics.median(row["compiled"] for row in rows),
                 "rss_mb": max(row["rss_mb"] for row in rows),
                 "failed_runs": sum(row["rc"] != 0 for row in rows)}
        if any("cache" in row for row in rows):
            entry["cache"] = {key: sum(row.get("cache", {}).get(key, 0) for row in rows)
                              for key in ("hits", "misses")}
        summary[label] = entry
    return summary


def choose(value: str, allowed: tuple[str, ...], what: str) -> list[str]:
    """Splits a comma-separated option and rejects names outside `allowed`."""
    chosen = value.split(",")
    for name in chosen:
        if name not in allowed:
            raise SystemExit(f"unknown {what} {name!r}; choose from {', '.join(allowed)}")
    return chosen


class Sccache:
    """One sccache server per session, restarted around a cache reset: its
    directory cannot be deleted under a running server."""

    def __init__(self, binary: str | None):
        self.binary = binary

    def stop(self, session: edits.EditSession) -> None:
        if self.binary and session.mode == "sccache":
            subprocess.run([self.binary, "--stop-server"], env=session.env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def start(self, session: edits.EditSession) -> None:
        if self.binary and session.mode == "sccache":
            bench.run([self.binary, "--start-server"], env=session.env,
                      stdout=subprocess.DEVNULL)


def bench_commands(args: argparse.Namespace) -> dict:
    modes = choose(args.modes, MODES, "mode")
    commands = choose(args.commands, edits.COMMANDS, "command")
    if "coverage" in commands and shutil.which("cargo-llvm-cov") is None:
        raise SystemExit("command 'coverage' needs cargo-llvm-cov (cargo install cargo-llvm-cov)")
    sccache = shutil.which("sccache")
    if "sccache" in modes and sccache is None:
        raise SystemExit("mode 'sccache' needs sccache on PATH (cargo install sccache)")
    tinymold = bench.tinymold_binary() if "tinymold" in modes else None
    edits.pin_cargo_config()
    server = Sccache(sccache)

    results = []
    names = args.project.split(",")
    projects = [p for p in bench.load_corpus(None, needs=("edits",)) if p["name"] in names]
    missing = set(names) - {p["name"] for p in projects}
    if missing:
        raise SystemExit(f"no corpus project with edits named {', '.join(sorted(missing))}")
    for project in projects:
        src = bench.checkout(project)
        bench.run(["cargo", "fetch", "--locked"], cwd=src, env=bench.STOCK_ENV,
                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        sessions = [edits.EditSession(project, src, mode, "default", [], tinymold, sccache,
                                      name=f"commands-{mode}") for mode in modes]
        samples = {(s.mode, c, case): [] for s in sessions for c in commands for case in CASES}
        try:
            for run in range(1, args.runs + 1):
                for command in commands:
                    for session in sessions:
                        server.stop(session)
                        session.reset()
                        server.start(session)
                        for case in CASES:
                            if case == "warm":
                                shutil.rmtree(session.target, ignore_errors=True)
                            done = session.run(command)
                            samples[(session.mode, command, case)].append(done)
                            walls = ", ".join(f"{k} {v['wall_s']}s" for k, v in done.items())
                            print(f"{project['name']} / run {run} / {session.mode} / "
                                  f"{case}: {walls}", file=sys.stderr)
        finally:
            for session in sessions:
                server.stop(session)
                session.reset()
        for session in sessions:
            for command in commands:
                results.append({"project": project["name"], "rev": project["rev"],
                                "mode": session.mode, "command": command,
                                **{case: aggregate(samples[(session.mode, command, case)])
                                   for case in CASES}})
    return {"benchmark": "commands", "pins": bench.pins(), "commands": commands,
            "modes": modes, "results": results}


def add_parser(sub) -> None:
    parser = sub.add_parser("commands", help="build, clippy, tests and coverage: cold, then "
                            "warm from the cache with the target directory deleted")
    parser.add_argument("--runs", type=int, default=3)
    # tokio's full test suite and bevy's graph would take hours per command
    # and mode; the default is the three projects the edit sequence runs.
    parser.add_argument("--project", default=",".join(DEFAULT_PROJECTS),
                        help="comma-separated corpus projects (default: %(default)s)")
    parser.add_argument("--modes", default="baseline,sccache,tinymold",
                        help=f"comma-separated, from: {', '.join(MODES)}")
    parser.add_argument("--commands", default=",".join(edits.COMMANDS),
                        help=f"comma-separated, from: {', '.join(edits.COMMANDS)}")
