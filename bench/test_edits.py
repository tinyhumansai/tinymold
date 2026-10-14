"""Tests for the edit-sequence benchmark's bookkeeping (bench/edits.py).

Run with: python3 -m unittest discover -s bench -p 'test_*.py'
"""

import os
import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench  # noqa: E402
import edits  # noqa: E402
import report  # noqa: E402


class Schedule(unittest.TestCase):
    def test_the_edit_kinds_take_turns_with_a_churn_step_every_n(self):
        plan = edits.schedule(10, churn_every=5, has_churn=True)
        self.assertEqual(plan, ["comment", "private_body", "public_signature", "comment",
                                "churn", "private_body", "public_signature", "comment",
                                "private_body", "churn"])

    def test_a_fresh_step_wins_over_churn(self):
        plan = edits.schedule(30, churn_every=10, has_churn=True, fresh_every=15)
        self.assertEqual([i + 1 for i, kind in enumerate(plan) if kind != "comment"
                          and kind not in edits.KINDS], [10, 15, 20, 30])
        self.assertEqual(plan[14], "fresh")
        self.assertEqual(plan[29], "fresh")

    def test_a_project_without_churn_only_edits(self):
        self.assertNotIn("churn", edits.schedule(20, churn_every=5, has_churn=False))
        self.assertNotIn("churn", edits.schedule(20, churn_every=0, has_churn=True))


class Sizes(unittest.TestCase):
    def test_a_hardlinked_file_is_counted_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            first = Path(tmp) / "a"
            first.write_bytes(b"x" * 10_000)
            os.link(first, Path(tmp) / "b")
            (Path(tmp) / "c").write_bytes(b"y" * 100)
            usage = edits.disk_usage(Path(tmp))
            self.assertEqual(usage["files"], 2)
            self.assertEqual(usage["apparent"], 10_100)
            self.assertGreaterEqual(usage["allocated"], 10_000)

    def test_a_missing_directory_is_empty(self):
        usage = edits.disk_usage(Path("/nonexistent/tinymold-bench"))
        self.assertEqual(usage, {"apparent": 0, "allocated": 0, "files": 0})

    def test_the_target_is_split_into_incremental_coverage_and_total(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp)
            for rel, size in (("debug/incremental/s/a.o", 300), ("debug/deps/x.rlib", 200),
                              ("llvm-cov-target/debug/deps/y.rlib", 50),
                              ("llvm-cov-target/debug/incremental/s/b.o", 7)):
                (target / rel).parent.mkdir(parents=True, exist_ok=True)
                (target / rel).write_bytes(b"z" * size)
            split = edits.target_split(target)
            self.assertEqual(split["total"]["apparent"], 557)
            self.assertEqual(split["coverage"]["apparent"], 57)
            self.assertEqual(split["incremental"]["apparent"], 307)


class Measure(unittest.TestCase):
    def test_peak_rss_is_normalized_to_bytes(self):
        self.assertEqual(edits.rss_bytes(2048, "Darwin"), 2048)
        self.assertEqual(edits.rss_bytes(2048, "Linux"), 2048 * 1024)

    def test_a_command_reports_its_exit_code_stderr_and_a_peak_rss(self):
        done = edits.measure([sys.executable, "-c", "import sys; sys.stderr.write('hi'); "
                              "sys.exit(3)"], Path.cwd(), dict(os.environ))
        self.assertEqual(done["rc"], 3)
        self.assertEqual(done["stderr"], "hi")
        self.assertGreater(done["rss"], 1 << 20)

    def test_checked_units_count_as_compiled(self):
        counts = edits.unit_counts("   Compiling a v1\n    Checking b v1\n       Fresh c v1\n")
        self.assertEqual(counts, {"compiled": 2, "fresh": 1})


class CumulativeEdits(unittest.TestCase):
    def test_cumulative_edits_pile_up_and_restore_to_the_original(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "lib.rs"
            path.write_text("fn f() {}\n")
            changes = bench.Edits(Path(tmp), {"comment": {"file": "lib.rs"}}, cumulative=True)
            changes.apply("comment", 1)
            changes.apply("comment", 2)
            text = path.read_text()
            self.assertIn("edit 1", text)
            self.assertIn("edit 2", text)
            changes.restore()
            self.assertEqual(path.read_text(), "fn f() {}\n")

    def test_plain_edits_replace_the_previous_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "lib.rs"
            path.write_text("fn f() {}\n")
            changes = bench.Edits(Path(tmp), {"comment": {"file": "lib.rs"}})
            changes.apply("comment", 1)
            changes.apply("comment", 2)
            self.assertNotIn("edit 1", path.read_text())


def step(n, edit, build, target, store=None):
    commands = {"build": {"wall_s": build, "rss_mb": 100.0 + n}}
    row = {"step": n, "edit": edit, "commands": commands,
           "target": {"total": {"allocated": target}, "incremental": {"allocated": 0},
                      "coverage": {"allocated": 0}}}
    if store is not None:
        row["store"] = {"allocated": store}
    return row


class Report(unittest.TestCase):
    def test_latency_skips_churn_and_reads_the_edit_after_a_fresh_build(self):
        mib = 2**20
        steps = [step(0, "clean", 9.0, 100 * mib, 50 * mib), step(1, "comment", 1.0, 110 * mib,
                 60 * mib), step(2, "churn", 5.0, 150 * mib, 90 * mib),
                 step(3, "fresh", 2.0, 120 * mib, 90 * mib),
                 step(4, "private_body", 3.0, 140 * mib, 100 * mib)]
        result = {"machine": {"os": "darwin", "arch": "arm64", "cpus": 8, "rustc": "rustc 1"},
                  "runs": 1, "date": "2026-10-15", "commands": ["build"], "schedule": "s",
                  "results": [{"project": "rg", "rev": "v1", "mode": "tinymold",
                               "steps": steps}]}
        with tempfile.TemporaryDirectory() as tmp, \
                unittest.mock.patch.object(report, "RESULTS", Path(tmp)):
            text = "\n".join(report.edits_section("edits-test", result))
            self.assertTrue((Path(tmp) / "edits-test-rg.svg").exists())
        # build median over the two edit steps (1.0 and 3.0), the fresh build,
        # and the edit after it
        self.assertIn("| rg v1 | tinymold | 2.00s | – | – | – | – | 2.00s | 3.00s |", text)
        # target/ grows (140 - 100) / 4 = 10 MiB a step, the store 12 or 13
        self.assertIn("| 140 MiB | 10 MiB |", text)
        self.assertIn("| 100 MiB | 12 MiB | 104 MiB |", text)


if __name__ == "__main__":
    unittest.main()
