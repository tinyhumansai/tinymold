#!/usr/bin/env python3
"""Benchmark OpenHuman's build in Linux Docker: GNU ld, mold, tinymold, sccache.

The sccache config runs twice: as cargo's dev profile builds by default, and
with CARGO_INCREMENTAL=0, which sccache needs to cache the workspace crates.

OpenHuman (github.com/tinyhumansai/openhuman) is a large workspace: about a
thousand packages, a dozen vendored submodules, and one big core library
under a binary. This builds its `openhuman-core` binary inside the
tinymold-bench image (bench/docker/Dockerfile) under three configurations and
the build states a developer or CI runner meets; bench/docker/openhuman.sh
lists them.

  run     build the image, copy a pinned OpenHuman checkout into a Docker
          volume, and run every case; output lands in
          bench/.work/openhuman-docker/
  report  turn that output into bench/results/openhuman-docker-<os>-<arch>.json
          and print the tables, with the cold builds' critical paths

Sources go into a volume rather than a bind mount, so the build reads and
writes the VM's own filesystem instead of crossing into the host's.

Usage:
  python3 bench/docker_openhuman.py run [--rev REV] [--runs N] [--configs ld,mold,tinymold]
  python3 bench/docker_openhuman.py report
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import platform
import statistics
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench  # noqa: E402
import profile_build  # noqa: E402

REPO = "git@github.com:tinyhumansai/openhuman.git"
IMAGE = "tinymold-bench"
VOLUME = "tinymold-openhuman"
CONTAINER = "tinymold-oh-bench"
SOURCE = bench.WORK / "openhuman"
OUT = bench.WORK / "openhuman-docker"
CONFIGS = ("ld", "mold", "tinymold", "sccache", "sccache-noinc")
CASES = ("cold", "noop", "edit_leaf", "edit_core", "warm", "second_path", "snapshot")
LABELS = {
    "cold": "Clean build, empty cache",
    "noop": "No-op rebuild",
    "edit_leaf": "Edit the binary's main.rs",
    "edit_core": "Edit the core library's lib.rs",
    "warm": "Clean target/, warm cache",
    "second_path": "Second checkout at another path",
    "snapshot": "Second checkout from a target/ snapshot",
}


def checkout(rev: str | None) -> str:
    """A shallow OpenHuman checkout with its submodules; returns its commit."""
    if not SOURCE.is_dir():
        bench.run(["git", "clone", "--quiet", "--depth", "1", "--recurse-submodules",
                   "--shallow-submodules", REPO, str(SOURCE)])
    if rev:
        bench.run(["git", "-C", str(SOURCE), "fetch", "--quiet", "--depth", "1", "origin", rev])
        bench.run(["git", "-C", str(SOURCE), "checkout", "--quiet", "FETCH_HEAD"])
        bench.run(["git", "-C", str(SOURCE), "submodule", "update", "--quiet", "--init",
                   "--recursive", "--depth", "1"])
    return bench.output(["git", "-C", str(SOURCE), "rev-parse", "--short", "HEAD"])


def load_volume() -> None:
    """Copies the checkout, without .git, node_modules, or target/, into the
    benchmark volume."""
    subprocess.run(["docker", "volume", "create", VOLUME], check=True,
                   stdout=subprocess.DEVNULL)
    tar = subprocess.Popen(["tar", "-C", str(SOURCE), "--no-xattrs", "--exclude=./.git",
                            "--exclude=*/.git", "--exclude=node_modules", "--exclude=./target",
                            "-cf", "-", "."], stdout=subprocess.PIPE,
                           env={"COPYFILE_DISABLE": "1", "PATH": "/usr/bin:/bin"})
    subprocess.run(["docker", "run", "--rm", "-i", "-v", f"{VOLUME}:/work", IMAGE, "sh", "-c",
                    "rm -rf /work/oh /work/oh-second /work/store && mkdir -p /work/oh"
                    " && tar -xf - -C /work/oh"], stdin=tar.stdout, check=True)
    tar.wait()


def docker_info() -> dict:
    info = json.loads(bench.output(["docker", "info", "--format", "{{json .}}"]))
    probe = bench.output(["docker", "run", "--rm", "-v", f"{VOLUME}:/work", IMAGE, "sh", "-c",
                          "rustc -V; mold --version | cut -d' ' -f1,2; ld --version | head -1;"
                          " df -T /work | tail -1 | awk '{print $2}'; sccache --version"]
                         ).splitlines()
    return {"docker": info.get("ServerVersion"), "cpus": info.get("NCPU"),
            "memory_bytes": info.get("MemTotal"), "kernel": info.get("KernelVersion"),
            "storage_driver": info.get("Driver"), "rustc": probe[0], "mold": probe[1],
            "ld": probe[2], "work_filesystem": probe[3], "sccache": probe[4]}


def cmd_run(args: argparse.Namespace) -> None:
    rev = checkout(args.rev)
    subprocess.run(["docker", "build", "-q", "-f", "bench/docker/Dockerfile", "-t", IMAGE, "."],
                   cwd=bench.ROOT, check=True, stdout=subprocess.DEVNULL)
    load_volume()
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "meta.json").write_text(json.dumps({"rev": rev, "runs": args.runs,
                                               "configs": args.configs}))
    subprocess.run(["docker", "rm", "-f", CONTAINER], stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL)
    subprocess.run(["docker", "run", "--name", CONTAINER, "-v", f"{VOLUME}:/work",
                    "-v", f"{OUT}:/out", "-e", f"RUNS={args.runs}",
                    "-e", f"CONFIGS={args.configs.replace(',', ' ')}", IMAGE,
                    "/opt/bench/openhuman.sh"], check=True)
    cmd_report(args)


def aggregate(rows: list[dict]) -> dict:
    """Medians per (config, case), with min and max wall times."""
    groups: dict[tuple[str, str], list[dict]] = {}
    for row in rows:
        groups.setdefault((row["config"], row["case"]), []).append(row)
    cells = {}
    for (config, case), samples in groups.items():
        walls = [s["wall"] for s in samples]
        samples.sort(key=lambda s: s["run"])
        cell = {"runs": len(samples), "wall": statistics.median(walls), "first": walls[0],
                "min": min(walls),
                "max": max(walls), "link": statistics.median(s["link"] for s in samples),
                "links": max(s["links"] for s in samples),
                "compiled": max(s["compiled"] for s in samples),
                "hits": sum(s["hits"] for s in samples),
                "misses": sum(s["misses"] for s in samples)}
        for extra in ("restore", "store_bytes"):
            if extra in samples[0]:
                cell[extra] = statistics.median(s[extra] for s in samples)
        cells.setdefault(config, {})[case] = cell
    return cells


NEW_PATH = ("second_path", "snapshot")


def cell_text(cell: dict | None, case: str) -> str:
    if not cell:
        return "-"
    text = f"{cell['wall']:.1f}s · link {cell['link']:.2f}s · {cell['compiled']} compiled"
    if cell["hits"] or cell["misses"]:
        text += f" · {cell['hits']}/{cell['hits'] + cell['misses']} hits"
    if "restore" in cell:
        text += f" · restore {cell['restore']:.1f}s"
    if case in NEW_PATH and cell["first"] > 2 * cell["wall"]:
        text += f" · **first run {cell['first']:.1f}s**"
    return text


def tables(result: dict) -> str:
    cells, machine = result["cells"], result["machine"]
    configs = [c for c in CONFIGS if c in cells]
    lines = [f"### OpenHuman `openhuman-core` in Docker ({machine['os']} {machine['arch']}, "
             f"{machine['cpus']} CPUs, {machine['memory_bytes'] / 2**30:.0f} GiB, "
             f"{machine['rustc']}, {result['date']})", "",
             f"OpenHuman {result['rev']}, dev profile, `-j {machine['cpus']}`, sources and "
             f"store on {machine['work_filesystem']} in a Docker volume ({machine['mold']}, "
             f"{machine['ld']}{', ' + machine['sccache'] if machine.get('sccache') else ''}; host: {result['host']}). Each cell: median wall time of "
             f"{result['runs']} runs (one for a cold build), link time, units compiled, and "
             "cache hits (summed over runs). `ld` and `mold` have no cache, so their warm and "
             "second-checkout builds would be their cold build. A second checkout's repeat runs "
             "reuse what its first run stored at that path, so the first run is shown when "
             "it differs.", "",
             "| Case | " + " | ".join(configs) + " |",
             "| --- | " + " | ".join("---:" for _ in configs) + " |"]
    for case in CASES:
        if not any(case in cells[c] for c in configs):
            continue
        row = [cell_text(cells[c].get(case), case) for c in configs]
        lines.append(f"| {LABELS[case]} | " + " | ".join(row) + " |")
    mold, ld = cells.get("mold", {}), cells.get("ld", {})
    for cached in ("tinymold", "sccache", "sccache-noinc"):
        speedups, tm = [], cells.get(cached, {})
        for case in CASES:
            base = mold.get("cold") if case in NEW_PATH + ("warm",) else mold.get(case)
            if base and case in tm:
                cell = tm[case]
                restore = cell.get("restore", 0.0)
                text = f"{LABELS[case].lower()}: {base['wall'] / (cell['wall'] + restore):.1f}x"
                if case in NEW_PATH and cell["first"] > 2 * cell["wall"]:
                    text += (f" (first run at a new path "
                             f"{base['wall'] / (cell['first'] + restore):.1f}x)")
                speedups.append(text)
        if speedups:
            lines += ["", f"{cached}+mold against plain mold, by median wall time plus any "
                      "snapshot restore; cached cases compare against the cold mold build: "
                      + "; ".join(speedups) + "."]
    if ld and mold:
        ratios = [f"{LABELS[c].lower()}: {ld[c]['link'] / mold[c]['link']:.1f}x"
                  for c in ("cold", "edit_leaf", "edit_core")
                  if c in ld and c in mold and mold[c]["link"] > 0]
        if ratios:
            lines.append("")
            lines.append("Link time, GNU ld over mold: " + "; ".join(ratios) + ".")
    return "\n".join(lines) + "\n"


def critical_paths(jobs: int) -> str:
    """The cold and warm builds' critical paths, from their --timings reports."""
    lines = []
    for name in ("mold-cold-1", "tinymold-cold-1", "sccache-cold-1", "tinymold-warm-1",
                 "sccache-warm-1", "sccache-noinc-warm-1", "mold-edit_core-1",
                 "tinymold-edit_core-1", "sccache-edit_core-1", "sccache-noinc-edit_core-1"):
        html = OUT / "timings" / f"{name}.html"
        if not html.exists():
            continue
        summary = profile_build.analyze(profile_build.timings_data(html.read_text()),
                                        jobs, None, [], [])
        path = summary["critical_path"]
        top = sorted(path, key=lambda step: step["on_path"], reverse=True)[:4]
        lines.append(f"- `{name}`: {summary['last_end']:.1f}s of units, average concurrency "
                     f"{summary['avg_concurrency']:.1f}; critical path {len(path)} units, "
                     f"{summary['critical_path_work']:.1f}s, led by "
                     + ", ".join(f"{s['label']} {s['on_path']:.1f}s" for s in top) + ".")
    return ("\nCritical paths (from each build's `--timings`, analyzed by "
            "`bench/profile_build.py`):\n\n"
            + "\n".join(lines) + "\n") if lines else ""


def cmd_report(_args: argparse.Namespace) -> None:
    rows = [json.loads(line) for line in (OUT / "results.jsonl").read_text().splitlines()
            if line.strip()]
    rows = [row for row in rows if row["case"] != "settle"]
    meta = json.loads((OUT / "meta.json").read_text()) if (OUT / "meta.json").exists() else {}
    if not meta.get("rev") and SOURCE.is_dir():
        meta["rev"] = bench.output(["git", "-C", str(SOURCE), "rev-parse", "--short", "HEAD"])
    info = docker_info()
    machine = {"os": "linux", "arch": bench.output(["docker", "run", "--rm", IMAGE, "uname",
                                                    "-m"]), **info}
    result = {"benchmark": "openhuman-docker", "date": dt.date.today().isoformat(),
              "rev": meta.get("rev", "unknown"), "runs": meta.get("runs", 3),
              "host": f"{platform.system().lower()} {platform.machine()}, "
                      f"{bench.os.cpu_count()} CPUs",
              "machine": machine, "cells": aggregate(rows), "samples": rows}
    path = bench.RESULTS / f"openhuman-docker-{machine['os']}-{machine['arch']}.json"
    path.write_text(json.dumps(result, indent=2) + "\n")
    print(tables(result) + critical_paths(machine["cpus"] or 1))
    print(f"Wrote {path}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run")
    run.add_argument("--rev", help="OpenHuman commit or branch (default: the checkout's HEAD)")
    run.add_argument("--runs", type=int, default=3)
    run.add_argument("--configs", default=",".join(CONFIGS))
    sub.add_parser("report")
    args = parser.parse_args()
    {"run": cmd_run, "report": cmd_report}[args.command](args)


if __name__ == "__main__":
    main()
