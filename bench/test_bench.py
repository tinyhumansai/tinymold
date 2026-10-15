"""Tests for bench.py's result bookkeeping and report.py's comparisons.

Run with: python3 -m unittest discover -s bench -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench  # noqa: E402
import report  # noqa: E402


def row(project, mode, case, wall=1.0):
    timing = {"median_s": wall, "min_s": wall, "max_s": wall}
    return {"project": project, "rev": "v1", "mode": mode, "case": case,
            "wall": timing, "link": timing}


MACHINE = {"os": "darwin", "arch": "arm64", "cpus": 8, "rustc": "rustc 1.99.0"}


class MergePrevious(unittest.TestCase):
    def test_an_experiments_run_keeps_other_variants_rows_and_settings(self):
        previous = {"benchmark": "experiments",
                    "variants": {"cgu-64": bench.VARIANTS["cgu-64"]},
                    "results": [row("rg", "baseline", "comment"),
                                row("rg", "baseline+cgu-64", "comment")]}
        result = {"benchmark": "experiments",
                  "variants": {"cgu-128": bench.VARIANTS["cgu-128"]},
                  "results": [row("rg", "baseline", "comment", 2.0),
                              row("rg", "baseline+cgu-128", "comment")]}
        merged = bench.merge_previous(result, previous, limited=False)
        self.assertEqual(sorted(r["mode"] for r in merged["results"]),
                         ["baseline", "baseline+cgu-128", "baseline+cgu-64"])
        baseline = [r for r in merged["results"] if r["mode"] == "baseline"]
        self.assertEqual(baseline[0]["wall"]["median_s"], 2.0)
        self.assertEqual(set(merged["variants"]), {"cgu-64", "cgu-128"})

    def test_other_benchmarks_merge_only_when_limited_to_a_project(self):
        previous = {"benchmark": "scenarios", "results": [row("bat", "baseline", "comment")]}
        fresh = {"benchmark": "scenarios", "results": [row("rg", "baseline", "comment")]}
        self.assertEqual(len(bench.merge_previous(dict(fresh), previous, limited=False)
                             ["results"]), 1)
        self.assertEqual(len(bench.merge_previous(dict(fresh), previous, limited=True)
                             ["results"]), 2)

    def test_a_limited_merge_keeps_each_rows_own_run_count(self):
        previous = {"benchmark": "retest", "runs": 5,
                    "results": [row("ripgrep", "cargo", "again")]}
        fresh = {"benchmark": "retest", "runs": 3,
                 "results": [row("tinymold", "cargo", "again")]}
        merged = bench.merge_previous(fresh, previous, limited=True)
        self.assertEqual({r["project"]: r["runs"] for r in merged["results"]},
                         {"ripgrep": 5, "tinymold": 3})
        self.assertEqual(merged["runs"], 3)

    def test_a_row_that_already_records_its_runs_keeps_them(self):
        previous = {"benchmark": "retest", "runs": 3,
                    "results": [dict(row("ripgrep", "cargo", "again"), runs=5)]}
        fresh = {"benchmark": "retest", "runs": 3,
                 "results": [row("tinymold", "cargo", "again")]}
        merged = bench.merge_previous(fresh, previous, limited=True)
        self.assertEqual({r["project"]: r["runs"] for r in merged["results"]},
                         {"ripgrep": 5, "tinymold": 3})

    def test_recorded_toolchains_are_kept_across_runs(self):
        previous = {"benchmark": "experiments", "variants": {},
                    "toolchains": {"nightly": "rustc 1.100.0-nightly"}, "results": []}
        result = {"benchmark": "experiments", "variants": {}, "results": []}
        merged = bench.merge_previous(result, previous, limited=False)
        self.assertEqual(merged["toolchains"], {"nightly": "rustc 1.100.0-nightly"})


class NightlyReference(unittest.TestCase):
    def test_a_nightly_only_variant_is_compared_with_nightly(self):
        self.assertEqual(bench.reference_variant("threads-8"), "nightly")
        self.assertEqual(bench.reference_variant("cranelift"), "nightly")

    def test_stable_variants_and_nightly_itself_are_compared_with_stable(self):
        self.assertIsNone(bench.reference_variant("cgu-64"))
        self.assertIsNone(bench.reference_variant("nightly"))

    def test_the_nightly_compiler_is_recorded_when_a_variant_uses_it(self):
        with mock.patch.object(bench, "output", return_value="rustc 1.100.0-nightly") as run:
            self.assertEqual(bench.toolchains(["cgu-64", "threads-8"]),
                             {"nightly": "rustc 1.100.0-nightly"})
            run.assert_called_once_with(["rustc", "+nightly", "-V"])
        with mock.patch.object(bench, "output") as run:
            self.assertEqual(bench.toolchains(["cgu-64"]), {})
            run.assert_not_called()


class Report(unittest.TestCase):
    def result(self, **extra):
        return {"machine": MACHINE, "runs": 3, "date": "2026-10-14", **extra}

    def test_a_variant_whose_settings_are_unknown_gets_no_change(self):
        result = self.result(variants={}, results=[row("rg", "baseline", "comment", 1.0),
                                                   row("rg", "baseline+threads-8", "comment", 0.5)])
        text = "\n".join(report.experiments_section(result))
        self.assertNotIn("-50%", text)

    def test_the_experiments_heading_names_the_nightly_compiler(self):
        result = self.result(variants={"nightly": bench.VARIANTS["nightly"]},
                             toolchains={"nightly": "rustc 1.100.0-nightly"},
                             results=[row("rg", "baseline", "comment")])
        self.assertIn("rustc 1.100.0-nightly", "\n".join(report.experiments_section(result)))

    def test_an_ab_against_nightly_compares_with_the_nightly_side(self):
        result = self.result(variant="threads-8", settings=bench.VARIANTS["threads-8"],
                             reference="nightly",
                             toolchains={"nightly": "rustc 1.100.0-nightly"},
                             results=[row("rg", "baseline+nightly", "comment", 1.0),
                                      row("rg", "baseline+threads-8", "comment", 0.5)])
        text = "\n".join(report.ab_section(result))
        self.assertIn("-50%", text)
        self.assertIn("baseline+nightly", text)
        self.assertIn("rustc 1.100.0-nightly", text)


if __name__ == "__main__":
    unittest.main()


class Retest(unittest.TestCase):
    def test_the_runner_variable_names_the_host_triple(self):
        with mock.patch.object(bench, "host_triple", return_value="aarch64-apple-darwin"):
            self.assertEqual(bench.runner_env(Path("/t/tinymold")),
                             {"CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER": "/t/tinymold run-test"})

    def test_runner_counts_read_the_test_lines_of_the_log(self):
        import tempfile
        with tempfile.TemporaryDirectory() as dir:
            log = Path(dir) / "log"
            log.write_text("hit rg 1.0ms\ntest-hit rg 0.9ms\ntest-hit grep 0.8ms\n"
                           "test-miss rg 900.0ms recorded in 1ms\ntest-pass - 0.1ms x\n")
            self.assertEqual(bench.runner_counts(log), {"replayed": 2, "ran": 1})
            self.assertEqual(bench.runner_counts(Path(dir) / "absent"),
                             {"replayed": 0, "ran": 0})

    def test_the_report_shows_each_rows_run_count_when_they_differ(self):
        def side(project, mode, runs):
            timing = {"median_s": 1.0, "min_s": 1.0, "max_s": 1.0}
            return {"project": project, "rev": "v1", "case": "again", "mode": mode,
                    "wall": timing, "replayed": 0, "ran": 0, "runs": runs}
        result = {"benchmark": "retest", "machine": MACHINE, "runs": 3, "date": "2026-10-15",
                  "results": [side("rg", "cargo", 5), side("rg", "run-test", 5),
                              side("tm", "cargo", 3), side("tm", "run-test", 3)]}
        table = "\n".join(report.retest_section(result))
        self.assertIn("3/5 runs by row", table)
        self.assertIn("| rg v1 | again | 5 |", table)
        self.assertIn("| tm v1 | again | 3 |", table)

    def test_the_result_records_the_settings_retest_configures(self):
        settings = bench.retest_settings()
        self.assertIn("tinymold", settings["rustc_wrapper"])
        self.assertIn("tinymold run-test", settings["cargo_runner"])
        self.assertNotIn("timing wrapper", settings["linker"].replace("no timing wrapper", ""))
        self.assertNotIn("sccache", " ".join(settings.values()))
        cargo_settings = bench.machine(bench.RETEST_CARGO_SETTINGS.format(jobs=8))["cargo_settings"]
        self.assertTrue(cargo_settings.startswith("RUSTC_WRAPPER=tinymold on both sides"))
        self.assertIn("no rustc wrapper", bench.machine()["cargo_settings"])

    def test_the_report_compares_both_sides_per_case(self):
        def side(mode, wall, replayed, ran):
            timing = {"median_s": wall, "min_s": wall, "max_s": wall}
            return {"project": "rg", "rev": "14.1.1", "case": "again", "mode": mode,
                    "wall": timing, "replayed": replayed, "ran": ran}
        result = {"benchmark": "retest", "machine": MACHINE, "runs": 3, "date": "2026-10-15",
                  "results": [side("cargo", 4.0, 0, 0), side("run-test", 1.0, 5, 0)]}
        table = "\n".join(report.retest_section(result))
        self.assertIn("| rg 14.1.1 | again | 4.00s (4.00–4.00) | 1.00s (1.00–1.00) | 5 / 0 | -75% |",
                      table)
