#!/usr/bin/env python3
"""Profile one cargo build and show where its wall time goes.

bench.py says how long a build takes; this says why. It runs one build of a
corpus project with cargo's `--timings`, a linker wrapper that records every
link, and (under `--mode tinymold`) the wrapper's own per-step trace, then
merges them into one timeline and reads it for gaps worth closing:

  - the critical path: the chain of units that decided the wall time, with
    the time each one spent waiting for a free job slot;
  - parallelism: average concurrency and CPU use, idle job-slot seconds, and
    the windows where few units ran, with the units that held them up;
  - the lower bound on wall time from the critical path and from total work
    over the job count, and how far the build is from it;
  - cargo's own time before the first unit and after the last;
  - linking, build scripts, and proc macros;
  - under tinymold, how long hashing, lookup, restore, and storing took.

With `--nightly`, cargo also splits every unit into frontend, codegen, and
link sections (`-Zsection-timings`), which makes the critical path exact
across pipelined `.rmeta` edges. `--self-profile` adds rustc's
`-Zself-profile` per crate, summarized by measureme's `summarize` and merged
into a Chrome trace by `crox` when they are installed:

  cargo install --git https://github.com/rust-lang/measureme --locked summarize crox

Output goes to bench/.work/profiles/<project>-<mode>-<case>/: `report.md`
(also printed), `summary.json`, and `trace.json`, which Perfetto
(https://ui.perfetto.dev) or chrome://tracing opens.

Usage:
  python3 bench/profile.py [--project NAME] [--case CASE] [--mode baseline|tinymold]
                           [--warm] [--nightly] [--self-profile]
  python3 bench/profile.py --from-html target/cargo-timings/cargo-timing.html

Only the Python standard library is used.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench  # noqa: E402  (the harness's checkout, corpus and cargo pinning)

PROFILES = bench.WORK / "profiles"
CASES = ("clean", "noop", "comment", "private_body", "public_signature")

# Like bench.LINK_TIMER, but records when each link ran and what it wrote, so
# links land on the timeline next to the units that asked for them.
LINK_TRACER = """#!/usr/bin/env perl
use strict;
use Time::HiRes qw(time);
my $real = $ENV{TINYMOLD_PROFILE_REAL_LINKER} || 'cc';
my $out = '-';
for my $i (0 .. $#ARGV - 1) { $out = $ARGV[$i + 1] if $ARGV[$i] eq '-o'; }
my $start = time;
my $status = system($real, @ARGV);
my $end = time;
if (my $log = $ENV{TINYMOLD_PROFILE_LINK_LOG}) {
    open(my $fh, '>>', $log) or die "cannot append to $log: $!";
    printf $fh "%.6f %.6f %s\\n", $start, $end, $out;
    close($fh);
}
exit($status == -1 ? 127 : $status >> 8);
"""


# ---------------------------------------------------------------- inputs


def timings_data(html: str) -> dict:
    """The data cargo embeds in a `--timings` report: UNIT_DATA, plus
    CONCURRENCY_DATA and CPU_USAGE when present."""
    data = {}
    for name in ("UNIT_DATA", "CONCURRENCY_DATA", "CPU_USAGE"):
        match = re.search(rf"const {name} = (.*?);\n", html, re.S)
        if match:
            data[name] = json.loads(match.group(1))
    if "UNIT_DATA" not in data:
        raise SystemExit("no UNIT_DATA in the timings report; is this a cargo --timings file?")
    return data


def kind_of(unit: dict) -> str:
    target = unit.get("target", "")
    if unit.get("mode") == "run-custom-build":
        return "build-script-run"
    if "build-script" in target:
        return "build-script"
    if '"bin"' in target:
        return "bin"
    if '"test"' in target or "(test)" in target:
        return "test"
    if '"example"' in target:
        return "example"
    return "lib"


def units_of(raw: list[dict]) -> dict[int, dict]:
    """Cargo's units, keyed by index, with their end, kind, label, the end of
    each section, and the unit that unblocked them.

    Cargo lists under `unblocked_units` (and `unblocked_rmeta_units`) only the
    units this one was the *last* dependency of, so every unit has at most one
    unblocker: the dependency it was waiting on when it became ready.
    """
    units = {}
    for raw_unit in raw:
        unit = dict(raw_unit)
        unit["end"] = unit["start"] + unit["duration"]
        unit["kind"] = kind_of(unit)
        target = unit.get("target", "").strip()
        unit["label"] = f"{unit['name']} {target}".strip()
        unit["sections"] = {name: span for name, span in (unit.get("sections") or [])}
        unit["unblocker"] = None
        units[unit["i"]] = unit
    for unit in units.values():
        for edge, ids in (("full", unit.get("unblocked_units", [])),
                          ("rmeta", unit.get("unblocked_rmeta_units", []))):
            for other in ids:
                if other in units:
                    units[other]["unblocker"] = (unit["i"], edge)
    return units


def rmeta_time(unit: dict) -> float | None:
    """When a unit's `.rmeta` was ready: the end of its frontend section, which
    only nightly's -Zsection-timings reports."""
    frontend = unit["sections"].get("frontend")
    return unit["start"] + frontend["end"] if frontend else None


def unlock_time(units: dict[int, dict], unit: dict) -> float:
    """When `unit` became ready to run: when its unblocker finished, or emitted
    its `.rmeta`. Without section timings an `.rmeta` time is unknown, so it is
    taken as the moment the unit started, bounded by the unblocker's span."""
    if unit["unblocker"] is None:
        return 0.0
    other_id, edge = unit["unblocker"]
    other = units[other_id]
    if edge == "full":
        return other["end"]
    rmeta = rmeta_time(other)
    if rmeta is None:
        rmeta = min(max(unit["start"], other["start"]), other["end"])
    return min(rmeta, unit["start"])


# ---------------------------------------------------------------- analysis


def critical_path(units: dict[int, dict]) -> list[dict]:
    """The chain of units that ended last, first unit first. Each step says how
    long it waited after becoming ready (`wait`) and how much of its run the
    next step had to wait for (`on_path`: up to its `.rmeta` for a pipelined
    edge, its whole run otherwise)."""
    if not units:
        return []
    current = max(units.values(), key=lambda unit: unit["end"])
    path, until = [], current["end"]
    while True:
        ready = unlock_time(units, current)
        path.append({"unit": current["i"], "label": current["label"], "kind": current["kind"],
                     "start": current["start"], "ready": ready,
                     "wait": max(0.0, current["start"] - ready),
                     "on_path": max(0.0, until - current["start"]),
                     "duration": current["duration"],
                     "edge": current["unblocker"][1] if current["unblocker"] else None,
                     "sections": {name: span["end"] - span["start"]
                                  for name, span in current["sections"].items()}})
        if current["unblocker"] is None:
            break
        until = ready
        current = units[current["unblocker"][0]]
    path.reverse()
    return path


def concurrency(units: dict[int, dict]) -> list[tuple[float, float, list[int]]]:
    """Intervals `(start, end, running unit ids)` covering the build, from the
    first unit's start to the last unit's end."""
    events = sorted({t for unit in units.values() for t in (unit["start"], unit["end"])})
    intervals = []
    for start, end in zip(events, events[1:]):
        mid = (start + end) / 2
        running = [i for i, unit in units.items() if unit["start"] <= mid < unit["end"]]
        intervals.append((start, end, running))
    return intervals


def cpu_between(cpu: list[list[float]], start: float, end: float) -> float | None:
    samples = [pct for t, pct in cpu if start <= t <= end]
    return statistics.fmean(samples) if samples else None


def low_parallelism_windows(intervals, jobs: int, cpu: list, limit: int = 8,
                            min_len: float = 0.05) -> list[dict]:
    """The longest stretches where at most a quarter of the job slots (and at
    least one) were busy, merged across adjacent intervals, longest first."""
    threshold = max(1, jobs // 4)
    windows: list[dict] = []
    for start, end, running in intervals:
        low = 0 < len(running) <= threshold
        if low and windows and windows[-1]["end"] == start and windows[-1]["open"]:
            window = windows[-1]
            window["end"] = end
            window["units"].update(running)
            window["slot_seconds"] += len(running) * (end - start)
        elif low:
            windows.append({"start": start, "end": end, "units": set(running), "open": True,
                            "slot_seconds": len(running) * (end - start)})
        elif windows:
            windows[-1]["open"] = False
    result = []
    for window in windows:
        length = window["end"] - window["start"]
        if length >= min_len:
            result.append({"start": window["start"], "end": window["end"], "length": length,
                           "avg_running": window["slot_seconds"] / length,
                           "units": sorted(window["units"]),
                           "cpu": cpu_between(cpu, window["start"], window["end"])})
    result.sort(key=lambda window: window["length"], reverse=True)
    return result[:limit]


def parse_links(text: str, t0: float) -> list[dict]:
    """Links recorded by the link tracer, as offsets from the build's start."""
    links = []
    for line in text.splitlines():
        start, end, out = line.split(" ", 2)
        links.append({"start": float(start) - t0, "end": float(end) - t0,
                      "duration": float(end) - float(start), "output": Path(out).name})
    return links


def parse_wrapper_trace(text: str) -> list[dict]:
    return [json.loads(line) for line in text.splitlines() if line.strip()]


def wrapper_summary(events: list[dict]) -> dict:
    """Per-step totals of the wrapper trace, and per-invocation overhead: the
    time each invocation spent outside rustc."""
    steps: dict[str, list[float]] = {}
    outcomes: dict[str, int] = {}
    by_pid: dict[int, dict] = {}
    for event in events:
        seconds = event["dur"] / 1e6
        steps.setdefault(event["name"], []).append(seconds)
        record = by_pid.setdefault(event["pid"], {"crate": event["args"]["crate"],
                                                  "outcome": event["args"]["outcome"]})
        record[event["name"]] = record.get(event["name"], 0.0) + seconds
    for record in by_pid.values():
        outcomes[record["outcome"]] = outcomes.get(record["outcome"], 0) + 1
        inner = record.get("rustc", 0.0) + record.get("pass_through", 0.0)
        record["overhead"] = record.get("invocation", 0.0) - inner
    return {
        "outcomes": outcomes,
        "steps": {name: {"count": len(values), "total": sum(values),
                         "median": statistics.median(values), "max": max(values)}
                  for name, values in steps.items()},
        "overhead_total": sum(record["overhead"] for record in by_pid.values()),
        "invocations": sorted(by_pid.values(), key=lambda r: r["overhead"], reverse=True),
    }


def analyze(data: dict, jobs: int, wall: float | None, links: list[dict],
            wrapper: list[dict]) -> dict:
    units = units_of(data["UNIT_DATA"])
    cpu = data.get("CPU_USAGE", [])
    intervals = concurrency(units)
    first = min((unit["start"] for unit in units.values()), default=0.0)
    last = max((unit["end"] for unit in units.values()), default=0.0)
    span = last - first
    busy = sum(len(running) * (end - start) for start, end, running in intervals)
    serial = sum(end - start for start, end, running in intervals if len(running) == 1)
    idle_slots = sum(max(0, jobs - len(running)) * (end - start)
                     for start, end, running in intervals)
    waiting = data.get("CONCURRENCY_DATA", [])
    saturated = sum(b["t"] - a["t"] for a, b in zip(waiting, waiting[1:]) if a["waiting"] > 0)
    path = critical_path(units)
    path_work = sum(step["on_path"] for step in path)
    path_wait = sum(step["wait"] for step in path)
    total_work = sum(unit["duration"] for unit in units.values())
    bound = max(first + path_work, first + total_work / jobs) if units else 0.0

    def total(kinds: tuple[str, ...]) -> float:
        return sum(unit["duration"] for unit in units.values() if unit["kind"] in kinds)

    def on_path(predicate) -> float:
        return sum(step["on_path"] for step in path if predicate(units[step["unit"]]))

    proc_macros = {unit["i"] for unit in units.values()
                   if "proc-macro" in unit.get("features", []) or unit["name"].endswith(
                       ("-impl", "_derive", "-derive", "-macros", "_macros"))}
    sections: dict[str, float] = {}
    for unit in units.values():
        for name, section in unit["sections"].items():
            sections[name] = sections.get(name, 0.0) + section["end"] - section["start"]
    return {
        "units": len(units),
        "jobs": jobs,
        "wall": wall,
        "head": first,
        "span": span,
        "last_end": last,
        "tail": (wall - last) if wall is not None else None,
        "total_work": total_work,
        "avg_concurrency": busy / span if span else 0.0,
        "avg_cpu": statistics.fmean(pct for _, pct in cpu) if cpu else None,
        "serial_seconds": serial,
        "idle_slot_seconds": idle_slots,
        "saturated_seconds": saturated,
        "critical_path": path,
        "critical_path_work": path_work,
        "critical_path_wait": path_wait,
        "lower_bound": bound,
        "build_scripts": {"total": total(("build-script", "build-script-run")),
                          "on_path": on_path(lambda u: u["kind"].startswith("build-script"))},
        "proc_macros": {"total": sum(units[i]["duration"] for i in proc_macros),
                        "on_path": on_path(lambda u: u["i"] in proc_macros)},
        "sections": sections,
        "slowest": sorted(({"label": u["label"], "kind": u["kind"], "start": u["start"],
                            "duration": u["duration"],
                            "sections": {n: s["end"] - s["start"]
                                         for n, s in u["sections"].items()}}
                           for u in units.values()),
                          key=lambda u: u["duration"], reverse=True)[:15],
        "windows": [dict(w, units=[units[i]["label"] for i in w["units"]])
                    for w in low_parallelism_windows(intervals, jobs, cpu)],
        "links": links,
        "link_total": sum(link["duration"] for link in links),
        "wrapper": wrapper_summary(wrapper) if wrapper else None,
    }


def opportunities(summary: dict) -> list[str]:
    """Plain observations tied to the numbers, largest first. They point at
    where to look, not at a verdict."""
    found: list[tuple[float, str]] = []
    wall = summary["wall"] or summary["last_end"]
    if not wall:
        return []

    def share(seconds: float) -> str:
        return f"{seconds:.2f}s, {100 * seconds / wall:.0f}% of wall"

    if summary["head"] > 0.05:
        found.append((summary["head"], f"cargo spent {share(summary['head'])} before the "
                      "first unit started (resolution, fingerprints, build plan)."))
    if summary["tail"] and summary["tail"] > 0.05:
        found.append((summary["tail"], f"cargo spent {share(summary['tail'])} after the last "
                      "unit finished."))
    if summary["critical_path_wait"] > 0.05:
        found.append((summary["critical_path_wait"], "critical-path units waited "
                      f"{share(summary['critical_path_wait'])} for a job slot after becoming "
                      "ready; raising their priority would shorten the build."))
    gap = wall - summary["lower_bound"]
    if gap > 0.1:
        found.append((gap, f"wall is {share(gap)} above the lower bound of "
                      f"{summary['lower_bound']:.2f}s (max of critical path and work/jobs)."))
    for step in summary["critical_path"]:
        codegen = step["sections"].get("codegen", 0.0)
        if codegen > 0.25 * wall:
            found.append((codegen, f"{step['label']} spends {share(codegen)} in codegen on "
                          "the critical path; more codegen units or a faster backend help."))
        if step["on_path"] > 0.25 * wall and not codegen:
            found.append((step["on_path"], f"{step['label']} holds the critical path for "
                          f"{share(step['on_path'])}; splitting it or caching it pays most."))
    scripts = summary["build_scripts"]["on_path"]
    if scripts > 0.05:
        found.append((scripts, f"build scripts sit on the critical path for {share(scripts)}."))
    macros = summary["proc_macros"]["on_path"]
    if macros > 0.05:
        found.append((macros, f"proc macros and their syn/quote chain hold the critical path "
                      f"for {share(macros)}."))
    if summary["link_total"] > 0.15 * wall:
        found.append((summary["link_total"], f"linking took {share(summary['link_total'])} "
                      f"across {len(summary['links'])} links."))
    if summary["windows"]:
        top = summary["windows"][0]
        found.append((top["length"], f"the longest low-parallelism window ran "
                      f"{top['length']:.2f}s at {top['avg_running']:.1f} units: "
                      f"{', '.join(top['units'][:4])}."))
    wrapper = summary["wrapper"]
    if wrapper and wrapper["overhead_total"] > 0.05:
        found.append((wrapper["overhead_total"], "the tinymold wrapper spent "
                      f"{wrapper['overhead_total']:.2f}s outside rustc in total (summed "
                      "across parallel invocations, so not all of it is wall time)."))
    found.sort(key=lambda item: item[0], reverse=True)
    return [text for _, text in found]


# ---------------------------------------------------------------- outputs


def chrome_trace(data: dict, t0: float, links: list[dict], wrapper: list[dict],
                 path: list[dict]) -> dict:
    """One Chrome trace of the build: cargo's units on lanes (critical-path
    units marked), their sections nested inside, links, wrapper steps, and
    concurrency and CPU counters. Times are microseconds since the epoch."""
    units = units_of(data["UNIT_DATA"])
    on_path = {step["unit"] for step in path}
    us = lambda seconds: int((t0 + seconds) * 1e6)  # noqa: E731
    events: list[dict] = [
        {"name": "process_name", "ph": "M", "pid": 1, "args": {"name": "cargo units"}},
        {"name": "process_name", "ph": "M", "pid": 2, "args": {"name": "linker"}},
        {"name": "process_name", "ph": "M", "pid": 3, "args": {"name": "tinymold wrapper"}},
    ]
    lanes: list[float] = []
    for unit in sorted(units.values(), key=lambda unit: unit["start"]):
        lane = next((n for n, free in enumerate(lanes) if free <= unit["start"]), len(lanes))
        if lane == len(lanes):
            lanes.append(0.0)
        lanes[lane] = unit["end"]
        critical = unit["i"] in on_path
        events.append({"name": unit["label"], "cat": "critical" if critical else unit["kind"],
                       "ph": "X", "pid": 1, "tid": lane, "ts": us(unit["start"]),
                       "dur": int(unit["duration"] * 1e6),
                       "args": {"kind": unit["kind"], "critical_path": critical}})
        for name, section in unit["sections"].items():
            if section["end"] > section["start"]:
                events.append({"name": name, "cat": "section", "ph": "X", "pid": 1,
                               "tid": lane, "ts": us(unit["start"] + section["start"]),
                               "dur": int((section["end"] - section["start"]) * 1e6)})
    for n, link in enumerate(links):
        events.append({"name": f"link {link['output']}", "cat": "link", "ph": "X", "pid": 2,
                       "tid": n, "ts": us(link["start"]), "dur": int(link["duration"] * 1e6)})
    wrapper_lanes: dict[int, int] = {}
    for event in wrapper:
        lane = wrapper_lanes.setdefault(event["pid"], len(wrapper_lanes))
        events.append(dict(event, pid=3, tid=lane,
                           name=f"{event['args']['crate']} {event['name']}"))
    for t, pct in data.get("CPU_USAGE", []):
        events.append({"name": "cpu %", "ph": "C", "pid": 1, "ts": us(t), "args": {"cpu": pct}})
    for sample in data.get("CONCURRENCY_DATA", []):
        events.append({"name": "units", "ph": "C", "pid": 1, "ts": us(sample["t"]),
                       "args": {"active": sample["active"], "waiting": sample["waiting"]}})
    return {"traceEvents": events, "displayTimeUnit": "ms"}


def fmt_sections(sections: dict[str, float]) -> str:
    return " · ".join(f"{name} {value:.2f}" for name, value in sections.items() if value >= 0.005)


def report(summary: dict, title: str, context: list[str]) -> str:
    s = summary
    wall = s["wall"] if s["wall"] is not None else s["last_end"]
    lines = [f"# Build profile: {title}", "", *context, ""]
    lines += ["## Where the time went", "",
              "| Part | Seconds | Share |", "| --- | ---: | ---: |"]
    parts = [("cargo before the first unit", s["head"]), ("units running", s["span"])]
    if s["tail"] is not None:
        parts.append(("cargo after the last unit", s["tail"]))
    for name, seconds in parts:
        lines.append(f"| {name} | {seconds:.2f} | {100 * seconds / wall:.0f}% |")
    lines.append(f"| **wall** | **{wall:.2f}** | |")
    lines += ["", "## Parallelism", "",
              f"- {s['units']} units, {s['total_work']:.2f}s of unit time on {s['jobs']} job "
              f"slots: average concurrency {s['avg_concurrency']:.1f}"
              + (f", average CPU {s['avg_cpu']:.0f}%" if s["avg_cpu"] is not None else "") + ".",
              f"- One unit running alone: {s['serial_seconds']:.2f}s. Idle job-slot time: "
              f"{s['idle_slot_seconds']:.1f} slot-seconds. Units ready but queued for a slot: "
              f"{s['saturated_seconds']:.2f}s.",
              f"- Lower bound on wall time: {s['lower_bound']:.2f}s (cargo's head plus the larger "
              f"of the critical path's {s['critical_path_work']:.2f}s and work/jobs "
              f"{s['total_work'] / s['jobs']:.2f}s). This build took {wall:.2f}s.", ""]
    lines += ["## Critical path", "",
              f"{len(s['critical_path'])} units; {s['critical_path_work']:.2f}s running and "
              f"{s['critical_path_wait']:.2f}s queued. `on path` is the part of a unit the next "
              "one waited for (up to its `.rmeta` across a pipelined edge).", "",
              "| Unit | Kind | Ready | Queued | On path | Run | Sections |",
              "| --- | --- | ---: | ---: | ---: | ---: | --- |"]
    for step in s["critical_path"]:
        lines.append(f"| {step['label']} | {step['kind']} | {step['ready']:.2f} | "
                     f"{step['wait']:.2f} | {step['on_path']:.2f} | {step['duration']:.2f} | "
                     f"{fmt_sections(step['sections'])} |")
    if s["windows"]:
        lines += ["", "## Low-parallelism windows", "",
                  f"Stretches with at most {max(1, s['jobs'] // 4)} of {s['jobs']} job slots busy.",
                  "", "| From | Length | Avg units | CPU | Units |",
                  "| ---: | ---: | ---: | ---: | --- |"]
        for window in s["windows"]:
            cpu = f"{window['cpu']:.0f}%" if window["cpu"] is not None else "-"
            lines.append(f"| {window['start']:.2f} | {window['length']:.2f} | "
                         f"{window['avg_running']:.1f} | {cpu} | {', '.join(window['units'])} |")
    lines += ["", "## Slowest units", "",
              "| Unit | Kind | Start | Run | Sections |", "| --- | --- | ---: | ---: | --- |"]
    for unit in s["slowest"]:
        lines.append(f"| {unit['label']} | {unit['kind']} | {unit['start']:.2f} | "
                     f"{unit['duration']:.2f} | {fmt_sections(unit['sections'])} |")
    lines += ["", "## Linking, build scripts, proc macros", "",
              f"- Links: {len(s['links'])}, {s['link_total']:.2f}s in total"
              + (": " + ", ".join(f"{link['output']} {link['duration']:.2f}s"
                                  for link in sorted(s["links"], key=lambda l: -l["duration"])[:5])
                 if s["links"] else "") + ".",
              f"- Build scripts (compile and run): {s['build_scripts']['total']:.2f}s, "
              f"{s['build_scripts']['on_path']:.2f}s on the critical path.",
              f"- Proc macros: {s['proc_macros']['total']:.2f}s, "
              f"{s['proc_macros']['on_path']:.2f}s on the critical path."]
    if s["sections"]:
        lines.append("- Unit time by section: " + fmt_sections(s["sections"]) + ".")
    wrapper = s["wrapper"]
    if wrapper:
        lines += ["", "## tinymold wrapper", "",
                  "Outcomes: " + ", ".join(f"{k} {v}" for k, v in wrapper["outcomes"].items())
                  + f". Time outside rustc: {wrapper['overhead_total']:.2f}s summed.", "",
                  "| Step | Count | Total | Median | Max |", "| --- | ---: | ---: | ---: | ---: |"]
        for name, step in wrapper["steps"].items():
            lines.append(f"| {name} | {step['count']} | {step['total']:.3f} | "
                         f"{step['median'] * 1000:.1f}ms | {step['max'] * 1000:.1f}ms |")
        lines += ["", "Most overhead per invocation:", ""]
        for record in wrapper["invocations"][:8]:
            lines.append(f"- {record['crate']} ({record['outcome']}): "
                         f"{record['overhead'] * 1000:.1f}ms outside rustc")
    hints = opportunities(s)
    if hints:
        lines += ["", "## Where to look", ""] + [f"- {hint}" for hint in hints]
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------- running


def link_tracer() -> Path:
    path = bench.WORK / "link-tracer"
    bench.WORK.mkdir(parents=True, exist_ok=True)
    if not path.exists() or path.read_text() != LINK_TRACER:
        path.write_text(LINK_TRACER)
        path.chmod(0o755)
    return path


def self_profile_outputs(directory: Path, out: Path) -> list[str]:
    """Runs measureme's tools over -Zself-profile output, when installed."""
    notes = []
    files = sorted(directory.glob("*.mm_profdata"))
    if not files:
        return ["- -Zself-profile wrote no profiles."]
    notes.append(f"- {len(files)} rustc self-profiles in `{directory}`.")
    if shutil.which("crox"):
        subprocess.run(["crox", "--dir", str(directory)], cwd=out, check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if (out / "chrome_profiler.json").exists():
            notes.append(f"- Every rustc's queries as one Chrome trace: "
                         f"`{out / 'chrome_profiler.json'}`.")
    else:
        notes.append("- Install measureme's `crox` to merge them into a Chrome trace.")
    if shutil.which("summarize"):
        largest = sorted(files, key=lambda f: f.stat().st_size, reverse=True)[:5]
        for profile in largest:
            done = subprocess.run(["summarize", "summarize", str(profile)], capture_output=True,
                                  text=True, check=False)
            (out / f"{profile.stem}.summary.txt").write_text(done.stdout)
        notes.append(f"- `summarize` tables for the {len(largest)} largest profiles: "
                     f"`{out}/*.summary.txt`.")
    else:
        notes.append("- Install measureme's `summarize` for per-query tables.")
    return notes


def profile_build(args: argparse.Namespace) -> None:
    project = bench.load_corpus(args.project)[0]
    src = bench.checkout(project)
    name = f"{project['name']}-{args.mode}-{args.case}"
    out = PROFILES / name
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    link_log, wrapper_log = out / "links.log", out / "wrapper.trace"

    toolchain = ["+nightly"] if args.nightly or args.self_profile else []
    tracer = link_tracer()
    env = dict(bench.STOCK_ENV, TINYMOLD_PROFILE_LINK_LOG=str(link_log))
    config = ["--config", f"target.{bench.host_triple()}.linker=\"{tracer}\""]
    if args.mode == "tinymold":
        tinymold = bench.tinymold_binary()
        env.update(RUSTC_WRAPPER=str(tinymold),
                   TINYMOLD_STORE=str(bench.WORK / "tinymold-store" / f"profile-{project['name']}"))
    unstable = ["-Zsection-timings"] if toolchain else []
    build = ["cargo", *toolchain, *bench.STOCK_CONFIG, *config, "build", "--locked",
             *bench.target_args(project)]
    if args.self_profile:
        profdir = out / "self-profile"
        profdir.mkdir()
        env["RUSTFLAGS"] = f"-Zself-profile={profdir} -Zself-profile-events=default"

    def quiet(cmd: list[str], extra_env: dict | None = None) -> None:
        done = subprocess.run(cmd, cwd=src, env=dict(env, **(extra_env or {})),
                              capture_output=True, text=True)
        if done.returncode:
            sys.stderr.write(done.stderr[-4000:])
            raise SystemExit(f"{project['name']}: build failed")

    edits = bench.Edits(src, project.get("edits", {}))
    if args.case == "clean" or args.warm:
        shutil.rmtree(src / "target", ignore_errors=True)
    if args.case != "clean" or (args.mode == "tinymold" and args.warm):
        # Prime: the build the profiled one starts from. Under tinymold with
        # --warm, a clean case profiles restoring every unit from the store.
        quiet(build, {"RUSTFLAGS": ""} if args.self_profile else None)
        if args.case == "clean":
            shutil.rmtree(src / "target", ignore_errors=True)
    if args.case in ("comment", "private_body", "public_signature"):
        if args.case not in edits.spec:
            raise SystemExit(f"{project['name']} has no {args.case} edit in corpus.json")
        edits.apply(args.case, int(time.time()))
    link_log.unlink(missing_ok=True)
    if args.mode == "tinymold":
        env["TINYMOLD_TRACE"] = str(wrapper_log)
    shutil.rmtree(src / "target" / "cargo-timings", ignore_errors=True)
    command = ["cargo", *toolchain, *unstable, *bench.STOCK_CONFIG, *config, "build", "--locked",
               "--timings", *bench.target_args(project)]
    try:
        t0 = time.time()
        done = subprocess.run(command, cwd=src, env=env, capture_output=True, text=True)
        wall = time.time() - t0
    finally:
        edits.restore()
    if done.returncode:
        sys.stderr.write(done.stderr[-4000:])
        raise SystemExit(f"{project['name']}: profiled build failed")
    reports = sorted((src / "target" / "cargo-timings").glob("cargo-timing-*.html"))
    if not reports:
        raise SystemExit("cargo wrote no timings report")
    shutil.copy(reports[-1], out / "cargo-timing.html")
    data = timings_data(reports[-1].read_text())
    links = parse_links(link_log.read_text(), t0) if link_log.exists() else []
    wrapper = parse_wrapper_trace(wrapper_log.read_text()) if wrapper_log.exists() else []
    jobs = os.cpu_count() or 1
    summary = analyze(data, jobs, wall, links, wrapper)
    machine = bench.machine()
    context = [f"- Project: {project['name']} {project['rev']} ({project.get('role', '')}).",
               f"- Case: {args.case}; mode: {args.mode}"
               + (", warm store" if args.warm else "") + "; toolchain: "
               + (output_version(toolchain) if toolchain else machine["rustc"]) + ".",
               f"- Machine: {machine['os']} {machine['arch']}, {machine['cpus']} CPUs; "
               f"cargo jobs {jobs}; filesystem {bench.fs_type(src)}.",
               f"- Command: `{' '.join(command)}`.",
               "- Times are seconds from cargo's start."]
    if not toolchain:
        context.append("- Stable cargo reports no frontend/codegen split; `--nightly` adds it "
                       "and makes the critical path exact across `.rmeta` edges.")
    if args.self_profile:
        context += self_profile_outputs(out / "self-profile", out)
    text = report(summary, f"{project['name']}, {args.case}, {args.mode}", context)
    (out / "report.md").write_text(text)
    (out / "summary.json").write_text(json.dumps(
        {"project": project["name"], "rev": project["rev"], "case": args.case,
         "mode": args.mode, "warm": args.warm, "nightly": bool(toolchain),
         "machine": machine, "command": command, **summary}, indent=2, default=list))
    (out / "trace.json").write_text(json.dumps(
        chrome_trace(data, t0, links, wrapper, summary["critical_path"])))
    print(text)
    print(f"Wrote {out}/: report.md, summary.json, trace.json (open in https://ui.perfetto.dev), "
          "cargo-timing.html")


def output_version(toolchain: list[str]) -> str:
    return bench.output(["rustc", *toolchain, "-V"])


def from_html(args: argparse.Namespace) -> None:
    path = Path(args.from_html)
    data = timings_data(path.read_text())
    summary = analyze(data, args.jobs or os.cpu_count() or 1, None, [], [])
    print(report(summary, path.name, [f"- Report: `{path}`.",
                                      "- No wall time, links or wrapper trace: analyzed from "
                                      "the timings report alone."]))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--project", default="ripgrep", help="corpus project (default ripgrep)")
    parser.add_argument("--case", default="clean", choices=CASES)
    parser.add_argument("--mode", default="baseline", choices=("baseline", "tinymold"))
    parser.add_argument("--warm", action="store_true",
                        help="under tinymold, prime the store first, so a clean case profiles "
                             "cache hits")
    parser.add_argument("--nightly", action="store_true",
                        help="build with cargo +nightly and -Zsection-timings")
    parser.add_argument("--self-profile", action="store_true",
                        help="also run rustc with -Zself-profile (implies --nightly)")
    parser.add_argument("--from-html", help="analyze an existing cargo --timings report instead")
    parser.add_argument("--jobs", type=int, help="job slots, with --from-html (default: CPUs)")
    args = parser.parse_args()
    if args.from_html:
        from_html(args)
    else:
        profile_build(args)


if __name__ == "__main__":
    main()
