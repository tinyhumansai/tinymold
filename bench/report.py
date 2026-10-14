#!/usr/bin/env python3
"""Render bench/results/*.json into bench/README.md and SVG charts.

Tables go between the `<!-- results:start -->` and `<!-- results:end -->`
markers in bench/README.md. Each link result also gets a horizontal bar chart,
bench/results/<name>.svg, in the style of mold's README chart: one bar per
linker, shorter is better. Only the Python standard library is used.

Usage:
  python3 bench/report.py
"""

from __future__ import annotations

import json
from html import escape
from pathlib import Path

BENCH = Path(__file__).resolve().parent
RESULTS = BENCH / "results"
README = BENCH / "README.md"
START, END = "<!-- results:start -->", "<!-- results:end -->"


def mib(n: int | None) -> str:
    return "n/a" if n is None else f"{n / 2**20:,.0f} MiB"


def describe(result: dict) -> str:
    m = result["machine"]
    return (f"{m['os']} {m['arch']}, {m['cpus']} CPUs, {m['rustc']}, "
            f"{result['runs']} runs, {result['date']}")


def chart(title: str, bars: list[tuple[str, float]], path: Path) -> None:
    """A light/dark-safe horizontal bar chart; shorter bars are faster."""
    width, label_w, bar_h, gap, top = 640, 150, 26, 10, 40
    longest = max(value for _, value in bars) or 1.0
    usable = width - label_w - 90
    height = top + len(bars) * (bar_h + gap) + 10
    rows = []
    for i, (label, value) in enumerate(bars):
        y = top + i * (bar_h + gap)
        w = max(2.0, usable * value / longest)
        rows.append(
            f'<text x="{label_w - 10}" y="{y + bar_h * 0.7}" text-anchor="end">{escape(label)}</text>'
            f'<rect x="{label_w}" y="{y}" width="{w:.1f}" height="{bar_h}" rx="3" fill="#4c78a8"/>'
            f'<text x="{label_w + w + 8:.1f}" y="{y + bar_h * 0.7}">{value:.2f}s</text>')
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
           f'font-family="system-ui, sans-serif" font-size="14" fill="#888">'
           f'<text x="0" y="22" font-weight="bold">{escape(title)}</text>'
           + "".join(rows) + "</svg>\n")
    path.write_text(svg)


def link_section(name: str, result: dict) -> list[str]:
    rows = result["results"]
    lines = [f"### Edit-rebuild loop ({describe(result)})", ""]
    for project in dict.fromkeys(r["project"] for r in rows):
        mine = [r for r in rows if r["project"] == project]
        svg = RESULTS / f"{name}-{project}.svg"
        chart(f"{project}: rebuild after a one-line edit (median, shorter is better)",
              [(r["linker"], r["edit_rebuild"]["median_s"]) for r in mine], svg)
        lines += [f"![{project} edit-rebuild by linker](results/{svg.name})", "",
                  "| Project | Linker | Edit → rebuilt binary (median) | min–max | Full build | Binary | target/ |",
                  "| --- | --- | ---: | ---: | ---: | ---: | ---: |"]
        for r in mine:
            e = r["edit_rebuild"]
            lines.append(
                f"| {project} {r['rev']} | {r['linker']} | {e['median_s']:.2f}s | "
                f"{e['min_s']:.2f}–{e['max_s']:.2f}s | {r['full_build_s']:.1f}s | "
                f"{mib(r['binary_bytes'])} | {mib(r['target_bytes'])} |")
        lines.append("")
    return lines


def baseline_section(result: dict) -> list[str]:
    rows = result["results"]
    lines = [f"### Baseline: plain cargo, default linker ({describe(result)})", "",
             "What every tinymold phase is measured against. Times are wall clock; "
             "loops report the median.", "",
             "| Scenario | " + " | ".join(f"{r['project']} {r['rev']}" for r in rows) + " |",
             "| --- |" + " ---: |" * len(rows)]

    def row(label: str, cell) -> None:
        lines.append(f"| {label} | " + " | ".join(cell(r) for r in rows) + " |")

    row("Clean build", lambda r: f"{r['clean_build_s']:.1f}s")
    row("No-op rebuild", lambda r: f"{r['noop_rebuild']['median_s']:.2f}s")
    row("One-line edit → rebuilt binary", lambda r: f"{r['edit_rebuild']['median_s']:.2f}s")
    row("Every source file touched, no content change",
        lambda r: f"{r['touch_all_rebuild']['median_s']:.2f}s")
    row("Build tests (after the binary)", lambda r: f"{r['build_tests_s']:.1f}s")
    row("Second worktree, same commit, clean build",
        lambda r: f"{r['second_worktree_build_s']:.1f}s")
    row("`target/` after binary + tests", lambda r: mib(r["target_bytes"]))
    row("Second worktree `deps/` artifacts",
        lambda r: mib(r["second_worktree_deps_bytes"]))
    row("…of which byte-identical to the first worktree's",
        lambda r: f"{mib(r['second_worktree_identical_deps_bytes'])} "
                  f"({100 * r['second_worktree_identical_deps_bytes'] / max(1, r['second_worktree_deps_bytes']):.0f}%)")
    return lines + [""]


def cache_section(result: dict) -> list[str]:
    rows = result["results"]
    lines = [f"### Clean builds: no cache, dry run, cached run ({describe(result)})", "",
             f"Cache: {result['cache']}, private directory and port. Each cell is the median "
             "clean build; hits and misses count rustc invocations.", "",
             "| Scenario | " + " | ".join(f"{r['project']} {r['rev']}" for r in rows) + " |",
             "| --- |" + " ---: |" * len(rows)]

    def row(label: str, cell) -> None:
        lines.append(f"| {label} | " + " | ".join(cell(r) for r in rows) + " |")

    def hits(stats: dict) -> str:
        total = stats["hits"] + stats["misses"]
        return f"{stats['hits']}/{total} hits" if total else "no cacheable calls"

    row("No cache", lambda r: f"{r['no_cache']['median_s']:.1f}s")
    row("Empty cache (dry run, fills it)", lambda r: f"{r['cold_cache']['median_s']:.1f}s")
    row("Warm cache (cached run)", lambda r: f"{r['warm_cache']['median_s']:.1f}s")
    row("Warm cache, second worktree at another path",
        lambda r: f"{r['second_worktree_warm_cache']['median_s']:.1f}s")
    row("Cached run hit rate", lambda r: hits(r["warm_stats"]))
    row("Second worktree hit rate", lambda r: hits(r["second_worktree_stats"]))
    row("Not cacheable (bins, proc-macros, incremental, …)",
        lambda r: str(r["warm_stats"]["non_cacheable"]))
    row("Cache size on disk", lambda r: mib(r["cache_bytes"]))
    return lines + [""]


CASE_LABELS = {
    "clean": "Clean build",
    "noop": "No-op rebuild",
    "comment": "Comment edit",
    "private_body": "Private function body edit",
    "public_signature": "Public signature edit",
    "second_worktree": "Second worktree, same commit",
    "touch_all": "Every source file touched, no content change",
    "branch_switch": "Branch switch A→B→A",
    "test_no_run": "cargo test --no-run",
}
KINDS = ("rlib", "rmeta", "debuginfo", "objects", "incremental", "executables", "other")


def scenarios_section(result: dict) -> list[str]:
    rows = result["results"]
    pins = result.get("pins", {})
    lines = [f"### Edit-loop scenarios ({describe(result)})", "",
             "Pinned: " + "; ".join(f"{k.replace('_', ' ')}: {v}" for k, v in pins.items()) + ".",
             "", "Each cell: median wall time, then median link time, then units "
             "compiled (cargo's count: a unit served from a cache still shows as "
             "compiled). Under sccache and tinymold, `hits` counts rustc invocations "
             "served from the cache across all runs. Under tinymold, cargo runs with "
             "`RUSTC_WRAPPER=tinymold`, the wall time includes `tinymold fresh restore` "
             "before and `fresh record` after every build, and the worktree and "
             "branch-switch cases include restoring a `target/` snapshot.", ""]
    for project in dict.fromkeys(r["project"] for r in rows):
        mine = [r for r in rows if r["project"] == project]
        modes = list(dict.fromkeys(r["mode"] for r in mine))
        first = mine[0]
        lines += [f"#### {project} {first['rev']}" + (f" ({first['role']})" if first["role"] else ""),
                  "", "| Case | " + " | ".join(modes) + " |", "| --- |" + " ---: |" * len(modes)]
        for case, label in CASE_LABELS.items():
            cells = []
            for mode in modes:
                found = [r for r in mine if r["mode"] == mode and r["case"] == case]
                if not found:
                    cells.append("–")
                    continue
                r = found[0]
                cell = (f"{r['wall']['median_s']:.2f}s · link {r['link']['median_s']:.2f}s · "
                        f"{r['compiled']:.0f} compiled")
                if "cache" in r and (r["cache"]["hits"] + r["cache"]["misses"]):
                    total = r["cache"]["hits"] + r["cache"]["misses"]
                    cell += f" · {r['cache']['hits']}/{total} hits"
                if case == "clean" and mode in ("sccache", "tinymold") and "first_wall_s" in r:
                    cell += f" · first (cold) run {r['first_wall_s']:.2f}s"
                cells.append(cell)
            lines.append(f"| {label} | " + " | ".join(cells) + " |")
        lines.append("")
        sized = [r for r in mine if r["mode"] == "baseline" and "target_kinds" in r]
        if sized:
            lines += ["`target/` by kind (baseline):", "",
                      "| After | " + " | ".join(KINDS) + " | total |",
                      "| --- |" + " ---: |" * (len(KINDS) + 1)]
            for r in sized:
                kinds = r["target_kinds"]
                lines.append(f"| {CASE_LABELS[r['case']]} | "
                             + " | ".join(mib(kinds[k]) for k in KINDS)
                             + f" | {mib(sum(kinds.values()))} |")
            lines.append("")
    return lines


def experiments_section(result: dict) -> list[str]:
    """Edit-loop experiments: each variant against its own reference, the stable
    baseline or, for nightly-only variants, the `nightly` variant."""
    rows = result["results"]
    variants = result.get("variants", {})
    lines = [f"### Edit-loop experiments ({describe(result)})", "",
             "Each variant is layered on the baseline. Cells: median wall time, the "
             "change against the reference (the stable baseline, or `baseline+nightly` "
             "for nightly variants), and median link time.", "",
             "| Variant | Settings |", "| --- | --- |"]
    for name, settings in variants.items():
        parts = list(settings.get("config", []))
        parts += [f"RUSTFLAGS {' '.join(settings['rustflags'])}"] if settings.get("rustflags") else []
        parts += [f"toolchain {settings['toolchain']}"] if settings.get("toolchain") else []
        lines.append(f"| {name} | " + "; ".join(f"`{part}`" for part in parts) + " |")
    lines += [""] + toolchain_lines(result)
    for project in dict.fromkeys(r["project"] for r in rows):
        mine = [r for r in rows if r["project"] == project]
        modes = list(dict.fromkeys(r["mode"] for r in mine))
        lines += [f"#### {project} {mine[0]['rev']}", "",
                  "| Case | " + " | ".join(modes) + " |", "| --- |" + " ---: |" * len(modes)]
        for case, label in CASE_LABELS.items():
            found = {r["mode"]: r for r in mine if r["case"] == case}
            if not found:
                continue
            cells = []
            for mode in modes:
                r = found.get(mode)
                if r is None:
                    cells.append("–")
                    continue
                name = mode.partition("+")[2]
                if name and name not in variants:
                    # Without its settings there is no telling which
                    # toolchain this column ran on, so no reference either.
                    reference = None
                else:
                    settings = variants.get(name, {})
                    nightly = settings.get("toolchain") == "nightly" and name != "nightly"
                    reference = found.get("baseline+nightly" if nightly else "baseline")
                cell = f"{r['wall']['median_s']:.2f}s"
                if reference is not None and reference is not r:
                    change = r["wall"]["median_s"] / reference["wall"]["median_s"] - 1
                    cell += f" ({change:+.0%})"
                cells.append(cell + f" · link {r['link']['median_s']:.2f}s")
            lines.append(f"| {label} | " + " | ".join(cells) + " |")
        lines.append("")
    return lines


def toolchain_lines(result: dict) -> list[str]:
    """The exact compilers behind non-default toolchains, when recorded."""
    found = result.get("toolchains") or {}
    return [f"Toolchain `{name}`: {version}." for name, version in found.items()] + \
        ([""] if found else [])


def ab_section(result: dict) -> list[str]:
    """Interleaved A/B: the baseline against one variant, per edit kind."""
    variant = result["variant"]
    settings = result.get("settings", {})
    parts = list(settings.get("config", []))
    parts += [f"RUSTFLAGS {' '.join(settings['rustflags'])}"] if settings.get("rustflags") else []
    parts += [f"toolchain {settings['toolchain']}"] if settings.get("toolchain") else []
    # Nightly-only variants run against the `nightly` variant, not stable.
    reference = result.get("reference")
    base_label = f"baseline+{reference}" if reference else "baseline"
    lines = [f"### A/B: {base_label} vs `{variant}` ({describe(result)})", "",
             f"`{variant}`: " + "; ".join(f"`{part}`" for part in parts) + ". Each edit is "
             "built by both sides back to back, in alternating order, each in its own "
             "target directory. Cells: median wall time (min–max).", "",
             *toolchain_lines(result),
             f"| Project | Edit | {base_label} | {variant} | change |",
             "| --- | --- | ---: | ---: | ---: |"]
    rows = result["results"]
    for project in dict.fromkeys(r["project"] for r in rows):
        for case in dict.fromkeys(r["case"] for r in rows if r["project"] == project):
            pair = {r["mode"]: r for r in rows if r["project"] == project and r["case"] == case}
            base, other = pair[base_label], pair[f"baseline+{variant}"]
            cell = lambda r: (f"{r['wall']['median_s']:.3f}s "
                              f"({r['wall']['min_s']:.2f}–{r['wall']['max_s']:.2f})")
            change = other["wall"]["median_s"] / base["wall"]["median_s"] - 1
            lines.append(f"| {project} {base['rev']} | {CASE_LABELS.get(case, case)} | "
                         f"{cell(base)} | {cell(other)} | {change:+.0%} |")
    return lines + [""]


def dedup_section(result: dict) -> list[str]:
    lines = [f"### Dedup probe: second worktree vs first ({describe(result)})", "",
             "Share of the second worktree's `target/` bytes that are byte-identical to "
             "something in the first's, at three granularities. Incremental state is "
             "excluded.", "",
             "| Project | Files | rlib members | DWARF sections |", "| --- | ---: | ---: | ---: |"]

    def share(level: dict) -> str:
        return (f"{100 * level['identical_bytes'] / max(1, level['bytes']):.0f}% "
                f"of {mib(level['bytes'])}")

    for r in result["results"]:
        lines.append(f"| {r['project']} {r['rev']} | {share(r['files'])} | "
                     f"{share(r['rlib_members'])} | {share(r['dwarf_sections'])} |")
    return lines + [""]


def ci_section(result: dict) -> list[str]:
    rows = result["results"]
    lines = [f"### A fresh CI runner ({describe(result)})", "",
             f"`{result['command']}` on a fresh checkout at the same path, with nothing on "
             "disk but what the job restores. Each cell is the median of restore plus build; "
             "downloading an archive from GitHub's cache is not included, so archive sizes "
             "are listed for that estimate.", "",
             "| Mode | " + " | ".join(f"{r['project']} {r['rev']}" for r in rows) + " |",
             "| --- |" + " ---: |" * len(rows)]

    def row(label: str, cell) -> None:
        lines.append(f"| {label} | " + " | ".join(cell(r) for r in rows) + " |")

    def timing(mode: str):
        def cell(r: dict) -> str:
            if mode not in r:
                return "n/a"
            stats = r[mode]["stats"]
            served = stats.get("hit", 0) + stats.get("remote", 0)
            total = served + stats.get("miss", 0)
            hits = f" ({served}/{total} hits)" if total else ""
            return f"{r[mode]['time']['median_s']:.1f}s{hits}"
        return cell

    row("Cold, no cache", timing("cold"))
    row("`target/` restored, sources touched", timing("target_cache"))
    row("tinymold store restored (GitHub backend)", timing("tinymold_github"))
    row("tinymold from bazel-remote only (remote backend)", timing("tinymold_remote"))
    row("`target/` archive", lambda r: mib(r["target_archive_bytes"]))
    row("tinymold store archive", lambda r: mib(r["store_archive_bytes"]))
    return lines + [""]


def cas_section(result: dict) -> list[str]:
    lines = [f"### Restoring a {result['artifact_mb']} MiB artifact ({describe(result)})", "",
             "| Method | Median | min–max |", "| --- | ---: | ---: |"]
    for r in result["results"]:
        s = r["restore"]
        lines.append(f"| {r['method']} | {s['median_s'] * 1000:.1f} ms | "
                     f"{s['min_s'] * 1000:.1f}–{s['max_s'] * 1000:.1f} ms |")
    return lines + [""]


def hit_section(result: dict) -> list[str]:
    """Cache hits on many-output units: one tinymold revision against another."""
    rows = result["results"]
    lines = [f"### Cache hits on many-output units ({describe(result)})", "",
             "Every source file touched, then rebuilt through the wrapper, so each "
             "workspace unit is a cache hit. Incremental units with "
             + ", ".join(f"`{value}`" for value in result["config"])
             + " keep one object file per codegen unit, so a hit places hundreds of "
             "files. Before is the `--against` revision, after is the checkout's; they "
             "take alternate turns on the same store. Restore times come from "
             "`TINYMOLD_TRACE`; cells are medians (min–max). A crate's row adds up all "
             "of its units, such as a library and a binary of the same name.", "",
             "| Project | What | before | after | change |",
             "| --- | --- | ---: | ---: | ---: |"]

    def cell(stats: dict, scale: float, unit: str, digits: int) -> str:
        return (f"{stats['median_s'] * scale:.{digits}f}{unit} "
                f"({stats['min_s'] * scale:.{digits}f}–{stats['max_s'] * scale:.{digits}f})")

    for project in dict.fromkeys(r["project"] for r in rows):
        mine = [r for r in rows if r["project"] == project]
        before, after = mine[0], mine[-1]
        name = f"{project} {before['rev']}<br>tinymold {before['tinymold']} → {after['tinymold']}"
        measures = [("Build wall time", lambda r: r["wall"], 1, "s", 2),
                    (f"All {before['hits']:.0f} restores, summed", lambda r: r["restore"],
                     1000, " ms", 0)]
        for unit in before["units"][:3]:
            def pick(r: dict, crate: str = unit["crate"]) -> dict:
                return next(u["restore"] for u in r["units"] if u["crate"] == crate)
            measures.append((f"`{unit['crate']}` restore ({unit['files']} files)", pick,
                             1000, " ms", 1))
        for label, pick, scale, unit, digits in measures:
            old, new = pick(before), pick(after)
            change = f"{new['median_s'] / old['median_s'] - 1:+.0%}" if old["median_s"] else "–"
            lines.append(f"| {name} | {label} | {cell(old, scale, unit, digits)} | "
                         f"{cell(new, scale, unit, digits)} | {change} |")
    return lines + [""]


def main() -> None:
    sections: list[str] = []
    order = {"scenarios": 0, "ab": 1, "experiments": 2, "baseline": 3, "cache": 4, "ci": 5,
             "hit": 6, "dedup": 7, "link": 8, "native": 9, "cas": 10}
    paths = sorted(RESULTS.glob("*.json"),
                   key=lambda p: (order.get(p.name.split("-")[0], 9), p.name))
    for path in paths:
        result = json.loads(path.read_text())
        if result["benchmark"] == "scenarios":
            sections += scenarios_section(result)
        elif result["benchmark"] == "experiments":
            sections += experiments_section(result)
        elif result["benchmark"] == "ab":
            sections += ab_section(result)
        elif result["benchmark"] == "dedup":
            sections += dedup_section(result)
        elif result["benchmark"] == "baseline":
            sections += baseline_section(result)
        elif result["benchmark"] == "cache":
            sections += cache_section(result)
        elif result["benchmark"] == "link":
            sections += link_section(path.stem, result)
        elif result["benchmark"] == "cas":
            sections += cas_section(result)
        elif result["benchmark"] == "ci":
            sections += ci_section(result)
        elif result["benchmark"] == "hit":
            sections += hit_section(result)
    body = "\n".join(sections).rstrip() or "No results recorded yet."

    text = README.read_text()
    before, rest = text.split(START, 1)
    _, after = rest.split(END, 1)
    README.write_text(f"{before}{START}\n\n{body}\n\n{END}{after}")
    print(f"rendered {len(list(RESULTS.glob('*.json')))} result file(s) into bench/README.md")


if __name__ == "__main__":
    main()
