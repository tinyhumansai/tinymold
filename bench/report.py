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


def cas_section(result: dict) -> list[str]:
    lines = [f"### Restoring a {result['artifact_mb']} MiB artifact ({describe(result)})", "",
             "| Method | Median | min–max |", "| --- | ---: | ---: |"]
    for r in result["results"]:
        s = r["restore"]
        lines.append(f"| {r['method']} | {s['median_s'] * 1000:.1f} ms | "
                     f"{s['min_s'] * 1000:.1f}–{s['max_s'] * 1000:.1f} ms |")
    return lines + [""]


def main() -> None:
    sections: list[str] = []
    for path in sorted(RESULTS.glob("*.json")):
        result = json.loads(path.read_text())
        if result["benchmark"] == "link":
            sections += link_section(path.stem, result)
        elif result["benchmark"] == "cas":
            sections += cas_section(result)
    body = "\n".join(sections).rstrip() or "No results recorded yet."

    text = README.read_text()
    before, rest = text.split(START, 1)
    _, after = rest.split(END, 1)
    README.write_text(f"{before}{START}\n\n{body}\n\n{END}{after}")
    print(f"rendered {len(list(RESULTS.glob('*.json')))} result file(s) into bench/README.md")


if __name__ == "__main__":
    main()
