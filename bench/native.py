#!/usr/bin/env python3
"""Native link benchmark: mold's README methodology on large C/C++ projects.

mold's README times the *final link* of Chromium, Clang, ClickHouse, Blender
and friends, isolated from the build that produced its inputs. This module
does the same on whatever machine it runs on:

  prepare <project>
         Clone, configure and fully build one project from corpus-native.json,
         untimed, under bench/.work/native/<project>/. Safe to re-run: every
         step either skips work that is done or is itself incremental.

  link [--runs N] [--project NAME] [--linkers a,b]
         Extract the final link command of every prepared project from its
         build system and replay it N times per available linker, in the
         directory the build system runs it in. The output is deleted before
         every run, one untimed warm-up run precedes the timed ones, and the
         original command is re-run at the end so the build tree is left as
         its build system made it.

Results merge into bench/results/native-<os>-<arch>.json (re-running one
project replaces only that project's rows) and each project gets a bar chart,
bench/results/native-<os>-<arch>-<project>.svg. See bench/NATIVE.md.

Only the Python standard library is used.

Usage:
  python3 bench/native.py prepare clang
  python3 bench/native.py link --runs 5 --project clang
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import time
from html import escape
from pathlib import Path

from bench import machine, summarize

BENCH = Path(__file__).resolve().parent
NATIVE = BENCH / ".work" / "native"
RESULTS = BENCH / "results"
CORPUS = BENCH / "corpus-native.json"

# A compiler driver token: clang, clang++, clang-23, gcc, g++-14, cc, c++, ...
DRIVER = re.compile(r"^(clang|clang\+\+|gcc|g\+\+|cc|c\+\+)(-[0-9.]+)?$")
INPUT_SUFFIXES = (".o", ".a", ".dylib", ".so", ".tbd", ".rlib", ".obj", ".lib")


def log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess:
    log("+ " + shlex.join(cmd))
    return subprocess.run(cmd, check=True, **kwargs)


def output(cmd: list[str], **kwargs) -> str:
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kwargs).stdout


def load_corpus(only: str | None) -> list[dict]:
    corpus = json.loads(CORPUS.read_text())["projects"]
    found = [p for p in corpus if only is None or p["name"] == only]
    if only and not found:
        raise SystemExit(f"no project named {only!r} in {CORPUS.name}")
    return found


# --------------------------------------------------------------------------
# prepare: one untimed full build per project


def clone(project: dict, dest: Path) -> None:
    if (dest / ".git").exists():
        return
    dest.parent.mkdir(parents=True, exist_ok=True)
    run(["git", "-c", "advice.detachedHead=false", "clone", "--depth", "1", "--single-branch",
         "--branch", project["rev"], project["repo"], str(dest)])


def ninja_build(build: Path, target: str, env: dict | None = None) -> None:
    # keeprsp: ninja deletes response files after a successful command, and
    # the replayed link command still needs them.
    run(["ninja", "-C", str(build), "-d", "keeprsp", target], env=env)


def prepare_cmake(project: dict, home: Path) -> None:
    src, build = home / "src", home / "build"
    clone(project, src)
    if not (build / "build.ninja").exists():
        run(["cmake", "-G", "Ninja", "-S", str(src / project.get("source_subdir", ".")),
             "-B", str(build), *project["cmake_args"]])
    ninja_build(build, project["target"])


def prepare_blender(project: dict, home: Path) -> None:
    src, build = home / "src", home / "build"
    clone(project, src)
    # Precompiled dependencies are git-lfs submodules under lib/<platform>.
    # make_update.py fetches them; --no-blender leaves the pinned tag alone.
    marker = home / ".libs-fetched"
    if not marker.exists():
        run([sys.executable, "build_files/utils/make_update.py", "--no-blender"], cwd=src)
        marker.touch()
    if not (build / "build.ninja").exists():
        run(["cmake", "-G", "Ninja", "-S", str(src), "-B", str(build), *project["cmake_args"]])
    ninja_build(build, project["target"])


def firefox_env(home: Path) -> dict:
    mozconfig = home / "mozconfig"
    return dict(os.environ, MOZCONFIG=str(mozconfig), MOZBUILD_STATE_PATH=str(home / "mozbuild"))


def prepare_firefox(project: dict, home: Path) -> None:
    src = home / "src"
    clone(project, src)
    lines = [*project["mozconfig"], f"mk_add_options MOZ_OBJDIR={home / 'obj'}"]
    (home / "mozconfig").write_text("\n".join(lines) + "\n")
    env = firefox_env(home)
    marker = home / ".bootstrapped"
    if not marker.exists():
        run([sys.executable, "mach", "--no-interactive", "bootstrap",
             "--application-choice=browser"], cwd=src, env=env)
        marker.touch()
    run([sys.executable, "mach", "build"], cwd=src, env=env)


def chromium_env(home: Path) -> dict:
    depot = home / "depot_tools"
    return dict(os.environ, PATH=f"{depot}{os.pathsep}{os.environ['PATH']}",
                DEPOT_TOOLS_UPDATE="0", DEPOT_TOOLS_METRICS="0")


def prepare_chromium(project: dict, home: Path) -> None:
    depot = home / "depot_tools"
    if not depot.is_dir():
        home.mkdir(parents=True, exist_ok=True)
        run(["git", "clone", "--depth", "1",
             "https://chromium.googlesource.com/chromium/tools/depot_tools.git", str(depot)])
    env = chromium_env(home)
    # A hand-written .gclient pinned to the release tag, instead of `fetch`,
    # so the sync can be shallow and land on exactly that tag.
    (home / ".gclient").write_text(
        "solutions = [{\n"
        f"  'name': 'src', 'url': '{project['repo']}@refs/tags/{project['rev']}',\n"
        "  'managed': False, 'custom_deps': {}, 'custom_vars': {},\n"
        "}]\n")
    marker = home / ".synced"
    if not marker.exists():
        run(["gclient", "sync", "--nohooks", "--no-history", "--shallow", "-D"], cwd=home, env=env)
        run(["gclient", "runhooks"], cwd=home, env=env)
        marker.touch()
    src, build = home / "src", home / "src" / "out" / "Debug"
    if not (build / "build.ninja").exists():
        run(["gn", "gen", str(build), "--args=" + " ".join(project["gn_args"])], cwd=src, env=env)
    run(["autoninja", "-C", str(build), "-d", "keeprsp", project["target"]], env=env)


PREPARE = {"cmake": prepare_cmake, "blender": prepare_blender,
           "firefox": prepare_firefox, "chromium": prepare_chromium}


def prepare(project: dict) -> None:
    home = NATIVE / project["name"]
    start = time.perf_counter()
    PREPARE[project["kind"]](project, home)
    (home / ".built").write_text(project["rev"] + "\n")
    log(f"{project['name']}: prepared in {time.perf_counter() - start:.0f}s")


# --------------------------------------------------------------------------
# link: extract the final link command and replay it


def driver_index(tokens: list[str]) -> int | None:
    for i, token in enumerate(tokens):
        if DRIVER.match(os.path.basename(token)):
            return i
    return None


def is_link(tokens: list[str]) -> bool:
    i = driver_index(tokens)
    return i is not None and "-o" in tokens[i:] and "-c" not in tokens[i:]


def split_command(command: str, cwd: Path) -> tuple[list[str], Path]:
    """The link segment of a shell command line, and the directory it runs in.

    CMake wraps links as `: && clang++ ... && :` and some generators prefix
    `cd dir &&`; the segments around the link are dropped, `cd`s are applied.
    """
    for segment in split_segments(command):
        if segment[:1] == ["cd"] and len(segment) == 2:
            cwd = (cwd / segment[1]).resolve()
        elif is_link(segment):
            return segment, cwd
    raise SystemExit(f"no link command in: {command[:200]}")


def ninja_link_command(build: Path, target: str, output_hint: str | None) -> str:
    lines = output(["ninja", "-C", str(build), "-t", "commands", target]).splitlines()
    candidates = [line for line in lines if any(
        is_link(seg) for seg in split_segments(line))]
    if output_hint:
        candidates = [line for line in candidates if output_hint in line]
    if not candidates:
        raise SystemExit(f"ninja lists no link command for {target} in {build}")
    return candidates[-1]


def split_segments(line: str) -> list[list[str]]:
    try:
        tokens = shlex.split(line)
    except ValueError:
        return []
    segments: list[list[str]] = [[]]
    for token in tokens:
        if token in ("&&", ";"):
            segments.append([])
        else:
            segments[-1].append(token)
    return segments


def firefox_link_command(home: Path) -> tuple[str, Path]:
    """libxul's link line, from a dry run of the recursive-make backend.

    `make -n -B` in toolkit/library/build prints every command that directory
    would run without running any of them; the libxul link is the one that
    invokes the compiler driver with `-o` and names XUL (macOS) or libxul.so.
    """
    build = home / "obj" / "toolkit" / "library" / "build"
    text = output(["make", "-n", "-B", "-C", str(build)], env=firefox_env(home))
    lines = [line for line in text.splitlines()
             if any(is_link(seg) for seg in split_segments(line))
             and re.search(r"(-o\s+\S*(XUL|libxul\.so))", line)]
    if not lines:
        raise SystemExit(f"no libxul link command in a dry run of {build}")
    return lines[-1], build


def link_command(project: dict) -> tuple[list[str], Path]:
    home = NATIVE / project["name"]
    if not (home / ".built").exists():
        raise SystemExit(f"{project['name']} is not prepared: "
                         f"python3 bench/native.py prepare {project['name']}")
    kind = project["kind"]
    if kind == "firefox":
        command, cwd = firefox_link_command(home)
        return split_command(command, cwd)
    build = home / "src" / "out" / "Debug" if kind == "chromium" else home / "build"
    command = ninja_link_command(build, project["target"], project.get("link_output"))
    return split_command(command, build)


def native_linkers() -> dict[str, str]:
    """Linkers on this machine, by name, as absolute paths for `--ld-path`."""
    found: dict[str, str] = {}
    system = platform.system()
    sysroot = output(["rustc", "--print", "sysroot"]).strip() if shutil.which("rustc") else ""
    gcc_ld = next(Path(sysroot).glob("lib/rustlib/*/bin/gcc-ld"), None) if sysroot else None
    if system == "Darwin":
        for name, tool in (("ld-prime", "ld"), ("ld-classic", "ld-classic")):
            try:
                found[name] = output(["xcrun", "-f", tool]).strip()
            except (subprocess.CalledProcessError, FileNotFoundError):
                pass
        lld = shutil.which("ld64.lld") or (gcc_ld and str(gcc_ld / "ld64.lld"))
        if lld and Path(lld).exists():
            found["lld"] = lld
    else:
        for name, tool in (("bfd", "ld.bfd"), ("gold", "ld.gold"), ("lld", "ld.lld"),
                           ("mold", "mold"), ("wild", "wild")):
            path = shutil.which(tool)
            if name == "lld" and not path and gcc_ld and (gcc_ld / "ld.lld").exists():
                path = str(gcc_ld / "ld.lld")
            if path:
                found[name] = path
    return found


def linker_version(path: str) -> str:
    for flag in ("-v", "--version"):
        proc = subprocess.run([path, flag], capture_output=True, text=True)
        text = (proc.stdout + proc.stderr).strip()
        if text:
            return text.splitlines()[0][:200]
    return "unknown"


def with_linker(tokens: list[str], linker: str) -> list[str]:
    """Insert the linker choice right after the compiler-driver token.

    Any `-fuse-ld=` or `--ld-path=` the build system chose is removed first.
    clang takes an absolute `--ld-path`; gcc's driver only knows `-fuse-ld`.
    """
    i = driver_index(tokens)
    assert i is not None, "split_command only returns segments with a driver"
    rest = [t for t in tokens[i + 1:]
            if not t.startswith(("-fuse-ld=", "--ld-path=", "-Wl,-fuse-ld="))]
    driver = os.path.basename(tokens[i])
    if driver.startswith("clang"):
        choice = [f"--ld-path={linker}"]
    else:
        choice = [f"-fuse-ld={os.path.basename(linker).removeprefix('ld.')}"]
    return [*tokens[:i + 1], *choice, *rest]


def output_path(tokens: list[str], cwd: Path) -> Path:
    for i, token in enumerate(tokens):
        if token == "-o" and i + 1 < len(tokens):
            return cwd / tokens[i + 1]
    raise SystemExit("link command has no -o")


def link_inputs(tokens: list[str], cwd: Path) -> dict:
    """Count and size the files a link command reads (objects, archives, libs)."""
    files: set[Path] = set()

    def add(arg: str) -> None:
        path = cwd / arg
        if arg.endswith(INPUT_SUFFIXES) or re.search(r"\.so\.\d", arg):
            if path.is_file():
                files.add(path.resolve())

    def filelist(arg: str) -> None:
        name = arg.split(",")[0]
        for line in (cwd / name).read_text().splitlines():
            if line.strip():
                add(line.strip())

    def walk(args: list[str]) -> None:
        it = iter(args)
        for token in it:
            if token.startswith("@") and (cwd / token[1:]).is_file():
                walk(shlex.split((cwd / token[1:]).read_text()))
            elif token == "-filelist":
                filelist(next(it, ""))
            elif token.startswith("-Wl,"):
                parts = token.split(",")[1:]
                for j, part in enumerate(parts):
                    if part == "-filelist" and j + 1 < len(parts):
                        filelist(parts[j + 1])
            elif token == "-o":
                next(it, None)
            elif not token.startswith("-"):
                add(token)

    walk(tokens)
    sizes = [(p, p.stat().st_size) for p in files]
    return {
        "files": len(sizes),
        "bytes": sum(size for _, size in sizes),
        "objects": sum(1 for p, _ in sizes if p.suffix in (".o", ".obj")),
        "archives": sum(1 for p, _ in sizes if p.suffix in (".a", ".lib", ".rlib")),
    }


def filesystem(path: Path) -> str:
    """The filesystem type holding `path` (apfs, ext4, xfs, tmpfs, ...)."""
    real = str(path.resolve())
    if platform.system() == "Darwin":
        best, kind = "", "unknown"
        for line in output(["/sbin/mount"]).splitlines():
            m = re.match(r".+? on (.+) \(([^,)]+)", line)
            if m and (real == m[1] or real.startswith(m[1].rstrip("/") + "/")) \
                    and len(m[1]) > len(best):
                best, kind = m[1], m[2]
        return kind
    try:
        return output(["stat", "-f", "-c", "%T", real]).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "unknown"


def remove(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    elif path.exists() or path.is_symlink():
        path.unlink()


def link_once(tokens: list[str], cwd: Path, out: Path) -> float:
    remove(out)
    start = time.perf_counter()
    proc = subprocess.run(tokens, cwd=cwd, stdout=subprocess.DEVNULL,
                          stderr=subprocess.PIPE, text=True)
    elapsed = time.perf_counter() - start
    if proc.returncode != 0:
        tail = "\n".join(proc.stderr.strip().splitlines()[-8:])
        raise RuntimeError(f"exit {proc.returncode}: {tail}")
    return elapsed


def driver_version(tokens: list[str], cwd: Path) -> str:
    i = driver_index(tokens)
    try:
        text = output([tokens[i], "--version"], cwd=cwd)
        return text.splitlines()[0]
    except (subprocess.CalledProcessError, FileNotFoundError, IndexError):
        return "unknown"


def rev_of(project: dict) -> str:
    home = NATIVE / project["name"]
    try:
        sha = output(["git", "-C", str(home / "src"), "rev-parse", "--short=12", "HEAD"]).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return project["rev"]
    return f"{project['rev']} ({sha})"


def bench_project(project: dict, linkers: dict[str, str], runs: int) -> list[dict]:
    original, cwd = link_command(project)
    out = output_path(original, cwd)
    inputs = link_inputs(original, cwd)
    common = {
        "project": project["name"],
        "rev": rev_of(project),
        "build_type": project["build_type"],
        "link_dir": str(cwd),
        "compiler": driver_version(original, cwd),
        "inputs": inputs,
        "output_reuse": "deleted before every run",
        "filesystem": filesystem(cwd),
        "threads": f"linker default ({os.cpu_count()} CPUs)",
        "warmup_runs": 1,
    }
    log(f"{project['name']}: {inputs['files']} inputs, {inputs['bytes'] / 2**30:.2f} GiB, "
        f"output {out.relative_to(cwd) if out.is_relative_to(cwd) else out}")
    rows = []
    try:
        for name, path in linkers.items():
            command = with_linker(original, path)
            row = {**common, "linker": name, "linker_path": path,
                   "linker_version": linker_version(path)}
            try:
                link_once(command, cwd, out)  # warm-up: page cache, linker binary
                samples = []
                for i in range(runs):
                    samples.append(link_once(command, cwd, out))
                    log(f"  {name} run {i + 1}/{runs}: {samples[-1]:.2f}s")
                row["link"] = summarize(samples)
                row["output_bytes"] = out.stat().st_size
            except RuntimeError as error:
                row["link"] = None
                row["output_bytes"] = None
                row["error"] = str(error)
                log(f"  {name} failed: {error}")
            rows.append(row)
    finally:
        # Leave the build tree as its build system made it.
        remove(out)
        subprocess.run(original, cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return rows


def results_path() -> Path:
    m = machine()
    return RESULTS / f"native-{m['os']}-{m['arch']}.json"


def bench_native(args: argparse.Namespace) -> dict:
    """Replay the final link of every prepared project once per linker.

    Rows for projects not benchmarked this time are carried over from the
    existing result file, so projects can be measured one at a time.
    """
    linkers = native_linkers()
    only = getattr(args, "linkers", None)
    if only:
        linkers = {k: v for k, v in linkers.items() if k in only.split(",")}
    log(f"linkers: {', '.join(linkers) or 'none'}")
    corpus = load_corpus(args.project)
    if args.project is None:
        corpus = [p for p in corpus if (NATIVE / p["name"] / ".built").exists()]
    fresh = []
    for project in corpus:
        fresh += bench_project(project, linkers, args.runs)
    done = {r["project"] for r in fresh}
    previous = []
    path = results_path()
    if path.exists():
        previous = [r for r in json.loads(path.read_text()).get("results", [])
                    if r["project"] not in done]
    order = [p["name"] for p in load_corpus(None)]
    rows = sorted(previous + fresh, key=lambda r: order.index(r["project"])
                  if r["project"] in order else len(order))
    return {"benchmark": "native", "results": rows}


# --------------------------------------------------------------------------
# rendering


def chart(title: str, bars: list[tuple[str, float]], path: Path) -> None:
    """A light/dark-safe horizontal bar chart; shorter bars are faster.

    Copied from report.py so this module renders on its own.
    """
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


def gib(n: int | None) -> str:
    return "n/a" if n is None else f"{n / 2**30:.2f} GiB"


def native_section(result: dict) -> list[str]:
    """Markdown for one native result: a mold-style table plus one chart per project."""
    rows = result["results"]
    m = result["machine"]
    stem = f"native-{m['os']}-{m['arch']}"
    linkers = list(dict.fromkeys(r["linker"] for r in rows))
    projects = list(dict.fromkeys(r["project"] for r in rows))
    lines = [f"### Final link of large C/C++ projects ({m['os']} {m['arch']}, {m['cpus']} CPUs, "
             f"{result.get('runs', '?')} runs, {result.get('date', '?')})", "",
             "mold's methodology: the final link command, extracted from the build system and "
             "replayed on its own; output deleted before each run; median wall clock.", "",
             "| Project | Output | Inputs | " + " | ".join(linkers) + " |",
             "| --- | ---: | ---: |" + " ---: |" * len(linkers)]
    for project in projects:
        mine = {r["linker"]: r for r in rows if r["project"] == project}
        first = next(iter(mine.values()))
        size = next((r["output_bytes"] for r in mine.values() if r.get("output_bytes")), None)
        cells = []
        for linker in linkers:
            r = mine.get(linker)
            cells.append("—" if r is None else "failed" if r.get("link") is None
                         else f"{r['link']['median_s']:.2f}s")
        inputs = first["inputs"]
        lines.append(f"| {project} {first['rev'].split(' ')[0]} ({first['build_type']}) | "
                     f"{gib(size)} | {inputs['files']:,} files, {gib(inputs['bytes'])} | "
                     + " | ".join(cells) + " |")
    lines.append("")
    for project in projects:
        mine = [r for r in rows if r["project"] == project and r.get("link")]
        if not mine:
            continue
        svg = RESULTS / f"{stem}-{project}.svg"
        chart(f"{project}: final link (median, shorter is better)",
              [(r["linker"], r["link"]["median_s"]) for r in mine], svg)
        lines += [f"![{project} final link by linker](results/{svg.name})", ""]
    notes = sorted({f"{r['linker']}: {r['linker_version']}" for r in rows})
    fs = sorted({r["filesystem"] for r in rows})
    lines += ["Linkers: " + "; ".join(notes) + ".",
              f"Filesystem: {', '.join(fs)}. Threads: each linker's default.", ""]
    failed = [r for r in rows if r.get("error")]
    for r in failed:
        lines.append(f"- {r['project']} / {r['linker']} failed: "
                     f"`{r['error'].splitlines()[-1][:160]}`")
    return lines + ([""] if failed else [])


# --------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    prep = sub.add_parser("prepare", help="clone, configure and build one project (untimed)")
    prep.add_argument("project")
    link = sub.add_parser("link", help="replay the final link per linker")
    link.add_argument("--runs", type=int, default=5)
    link.add_argument("--project", help="only this project")
    link.add_argument("--linkers", help="comma-separated subset, e.g. ld-prime,lld")
    sub.add_parser("render", help="print the Markdown section and write the charts")
    args = parser.parse_args()

    if args.command == "prepare":
        for project in load_corpus(args.project):
            prepare(project)
        return
    path = results_path()
    if args.command == "render":
        print("\n".join(native_section(json.loads(path.read_text()))))
        return

    result = bench_native(args)
    result["machine"] = {
        **machine(),
        "cpu_model": cpu_model(),
        "memory_bytes": memory_bytes(),
    }
    result["date"] = dt.date.today().isoformat()
    result["runs"] = args.runs
    RESULTS.mkdir(exist_ok=True)
    path.write_text(json.dumps(result, indent=2) + "\n")
    native_section(result)  # writes the charts
    log(f"wrote bench/results/{path.name}")


def cpu_model() -> str:
    try:
        if platform.system() == "Darwin":
            return output(["sysctl", "-n", "machdep.cpu.brand_string"]).strip()
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except (OSError, subprocess.CalledProcessError):
        pass
    return platform.processor() or "unknown"


def memory_bytes() -> int | None:
    try:
        if platform.system() == "Darwin":
            return int(output(["sysctl", "-n", "hw.memsize"]))
        return os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    except (OSError, ValueError, subprocess.CalledProcessError):
        return None


if __name__ == "__main__":
    main()
