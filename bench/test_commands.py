"""Tests for the per-command cache benchmark's bookkeeping (bench/commands.py)
and its report.

Run with: python3 -m unittest discover -s bench -p 'test_*.py'
"""

import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench  # noqa: E402
import commands  # noqa: E402
import report  # noqa: E402


def row(wall, compiled=10, rc=0, hits=None, misses=None):
    sample = {"wall_s": wall, "rc": rc, "rss_mb": 100.0 + wall, "link_s": wall / 10,
              "links": 1, "compiled": compiled, "fresh": 0}
    if hits is not None:
        sample["cache"] = {"hits": hits, "misses": misses}
    return sample


class Aggregate(unittest.TestCase):
    def test_runs_fold_into_a_median_per_label_with_hits_summed(self):
        samples = [{"test_build": row(4.0, hits=9, misses=1), "test_run": row(2.0, rc=101)},
                   {"test_build": row(2.0, hits=10, misses=0), "test_run": row(3.0)},
                   {"test_build": row(3.0, hits=8, misses=2), "test_run": row(1.0)}]
        summary = commands.aggregate(samples)
        self.assertEqual(list(summary), ["test_build", "test_run"])
        self.assertEqual(summary["test_build"]["wall"]["median_s"], 3.0)
        self.assertEqual(summary["test_build"]["cache"], {"hits": 27, "misses": 3})
        self.assertNotIn("cache", summary["test_run"])
        self.assertEqual(summary["test_run"]["failed_runs"], 1)
        self.assertEqual(summary["test_run"]["rss_mb"], 103.0)

    def test_an_unknown_mode_or_command_is_rejected(self):
        self.assertEqual(commands.choose("build,clippy", commands.edits.COMMANDS, "command"),
                         ["build", "clippy"])
        with self.assertRaises(SystemExit):
            commands.choose("build,lint", commands.edits.COMMANDS, "command")
        with self.assertRaises(SystemExit):
            commands.choose("ccache", commands.MODES, "mode")


class MergePrevious(unittest.TestCase):
    def test_a_limited_run_keeps_the_other_commands_of_the_same_mode(self):
        previous = {"benchmark": "commands", "results": [
            {"project": "rg", "mode": "tinymold", "command": "build", "old": True},
            {"project": "rg", "mode": "tinymold", "command": "clippy", "old": True}]}
        result = {"benchmark": "commands", "results": [
            {"project": "rg", "mode": "tinymold", "command": "clippy", "old": False}]}
        merged = bench.merge_previous(result, previous, limited=True)
        self.assertEqual([(r["command"], r["old"]) for r in merged["results"]],
                         [("build", True), ("clippy", False)])


def entry(cold, warm, hits=None):
    def case(wall):
        summary = {"wall": bench.summarize([wall])}
        if hits is not None:
            summary["cache"] = {"hits": hits, "misses": 1}
        return {"clippy": summary}
    return {"cold": case(cold), "warm": case(warm)}


RESULT = {"benchmark": "commands", "machine": {"os": "darwin", "arch": "arm64", "cpus": 8,
                                              "rustc": "rustc 1"},
          "runs": 3, "date": "2026-10-15", "modes": ["baseline", "sccache", "tinymold"],
          "results": [{"project": "rg", "rev": "v1", "mode": "baseline", "command": "clippy",
                       **entry(6.0, 5.8)},
                      {"project": "rg", "rev": "v1", "mode": "sccache", "command": "clippy",
                       **entry(7.0, 5.0)},
                      {"project": "rg", "rev": "v1", "mode": "tinymold", "command": "clippy",
                       **entry(8.0, 2.0, hits=47)}]}


class Report(unittest.TestCase):
    def test_the_section_shows_cold_and_warm_per_cache_and_the_speed_up(self):
        with tempfile.TemporaryDirectory() as tmp, \
                unittest.mock.patch.object(report, "RESULTS", Path(tmp)):
            text = "\n".join(report.commands_section("commands-test", RESULT))
            self.assertTrue((Path(tmp) / "commands-test.svg").exists())
        self.assertIn("| rg v1 | clippy | 6.00s | 7.00s | 5.00s | 8.00s | "
                      "2.00s (47/48 hits) | 3.0× |", text)

    def test_the_root_readme_table_is_replaced_between_its_markers(self):
        with tempfile.TemporaryDirectory() as tmp:
            readme = Path(tmp) / "README.md"
            readme.write_text(f"intro\n{report.COMMANDS_START}\nstale table\n{report.COMMANDS_END}\nend\n")
            with unittest.mock.patch.object(report, "ROOT_README", readme):
                report.render_root_readme(report.commands_summary("commands-x", RESULT))
            text = readme.read_text()
        self.assertNotIn("stale table", text)
        self.assertIn("| rg v1 | `clippy` | 6.00s | 2.00s | 3.0× |", text)
        self.assertTrue(text.startswith("intro\n") and text.endswith("end\n"))


if __name__ == "__main__":
    unittest.main()
