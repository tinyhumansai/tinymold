"""Tests for profile_build.py's analysis, on hand-built cargo timings.

Run with: python3 -m unittest discover -s bench -p 'test_*.py'
"""

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import profile_build as pb  # noqa: E402


def unit(i, name, start, duration, target="", mode="todo", full=(), rmeta=(), sections=None,
         features=()):
    return {"i": i, "name": name, "version": "1.0.0", "mode": mode, "target": target,
            "features": list(features), "start": start, "duration": duration,
            "unblocked_units": list(full), "unblocked_rmeta_units": list(rmeta),
            "sections": sections}


# A build script compiles, runs, and unblocks its library; the library's
# .rmeta unblocks the binary, which also waits a little for a slot. An
# unrelated leaf runs alongside, and one fresh unit did no work.
UNITS = [
    unit(0, "libc", 0.1, 0.5, target=" build-script", full=[1]),
    unit(1, "libc", 0.6, 2.0, target=" build-script (run)", mode="run-custom-build", full=[2]),
    unit(2, "libc", 2.6, 1.0, rmeta=[3],
         sections=[["frontend", {"start": 0.0, "end": 0.4}],
                   ["codegen", {"start": 0.4, "end": 1.0}]]),
    unit(3, "app", 3.1, 1.5, target=' app "bin"',
         sections=[["frontend", {"start": 0.0, "end": 0.3}],
                   ["codegen", {"start": 0.3, "end": 1.3}],
                   ["link", {"start": 1.3, "end": 1.5}]]),
    unit(4, "leaf", 0.1, 0.4),
    unit(5, "fresh", 0.0, 0.0),
]
HTML = ("<script>\nconst UNIT_DATA = " + json.dumps(UNITS) + ";\n"
        "const CONCURRENCY_DATA = " + json.dumps(
            [{"t": 0.1, "active": 2, "waiting": 0, "inactive": 3},
             {"t": 3.0, "active": 0, "waiting": 1, "inactive": 1},
             {"t": 3.1, "active": 1, "waiting": 0, "inactive": 0}]) + ";\n"
        "const CPU_USAGE = [[1.0, 20.0], [3.5, 80.0]];\n</script>")


class Timings(unittest.TestCase):
    def test_reads_the_data_cargo_embeds(self):
        data = pb.timings_data(HTML)
        self.assertEqual(len(data["UNIT_DATA"]), 6)
        self.assertEqual(data["CPU_USAGE"], [[1.0, 20.0], [3.5, 80.0]])

    def test_rejects_a_page_without_unit_data(self):
        with self.assertRaises(SystemExit):
            pb.timings_data("<html></html>")

    def test_drops_fresh_units_and_names_kinds(self):
        units = pb.units_of(UNITS)
        self.assertNotIn(5, units)
        self.assertEqual([units[i]["kind"] for i in range(5)],
                         ["build-script", "build-script-run", "lib", "bin", "lib"])
        self.assertEqual(units[1]["label"], "libc build-script (run)")
        self.assertEqual(units[3]["unblocker"], (2, "rmeta"))


class CriticalPath(unittest.TestCase):
    def test_follows_unblockers_from_the_last_unit(self):
        path = pb.critical_path(pb.units_of(UNITS))
        self.assertEqual([step["unit"] for step in path], [0, 1, 2, 3])

    def test_a_pipelined_edge_counts_only_up_to_the_rmeta(self):
        path = pb.critical_path(pb.units_of(UNITS))
        lib = path[2]
        self.assertAlmostEqual(lib["on_path"], 0.4)  # frontend end, not the full 1.0s
        app = path[3]
        self.assertAlmostEqual(app["ready"], 3.0)
        self.assertAlmostEqual(app["wait"], 0.1)

    def test_without_sections_an_rmeta_edge_is_taken_as_no_wait(self):
        raw = [dict(u, sections=None) for u in UNITS]
        path = pb.critical_path(pb.units_of(raw))
        self.assertAlmostEqual(path[3]["wait"], 0.0)
        self.assertAlmostEqual(path[2]["on_path"], 0.5)

    def test_roots_are_ready_when_the_first_unit_starts(self):
        path = pb.critical_path(pb.units_of(UNITS))
        self.assertAlmostEqual(path[0]["ready"], 0.1)
        self.assertAlmostEqual(path[0]["wait"], 0.0)


class Analysis(unittest.TestCase):
    def setUp(self):
        self.summary = pb.analyze(pb.timings_data(HTML), jobs=4, wall=4.8,
                                  links=[{"start": 4.4, "end": 4.6, "duration": 0.2,
                                          "output": "deps/app"}],
                                  wrapper=[])

    def test_splits_wall_into_head_units_and_tail(self):
        s = self.summary
        self.assertAlmostEqual(s["head"], 0.1)
        self.assertAlmostEqual(s["span"], 4.5)
        self.assertAlmostEqual(s["tail"], 0.2)

    def test_measures_parallelism_and_the_lower_bound(self):
        s = self.summary
        self.assertAlmostEqual(s["total_work"], 5.4)
        self.assertAlmostEqual(s["serial_seconds"], 0.1 + 2.0 + 0.5 + 1.0)
        self.assertAlmostEqual(s["critical_path_work"], 0.5 + 2.0 + 0.4 + 1.5)
        self.assertAlmostEqual(s["lower_bound"], 0.1 + 4.4)
        self.assertAlmostEqual(s["build_scripts"]["on_path"], 2.5)

    def test_finds_the_low_parallelism_window_and_who_held_it(self):
        window = self.summary["windows"][0]
        self.assertAlmostEqual(window["start"], 0.5)
        self.assertAlmostEqual(window["length"], 2.6)  # 0.5 to 3.1, before app starts
        self.assertTrue(window["units"][0].startswith("libc build-script (run)"))

    def test_names_the_largest_gaps(self):
        hints = pb.opportunities(dict(self.summary, os="darwin"))
        self.assertTrue(any("build scripts sit on the critical path" in h for h in hints))
        self.assertTrue(any("Developer Tools" in h for h in hints))
        self.assertFalse(any("Developer Tools" in h
                             for h in pb.opportunities(dict(self.summary, os="linux"))))

    def test_renders_a_report_and_a_trace(self):
        text = pb.report(self.summary, "demo", ["- context"])
        self.assertIn("## Critical path", text)
        self.assertIn("| libc build-script (run) | build-script-run |", text)
        trace = pb.chrome_trace(pb.timings_data(HTML), 1000.0, self.summary["links"], [],
                                self.summary["critical_path"])
        names = [e["name"] for e in trace["traceEvents"] if e["ph"] == "X"]
        self.assertIn('app app "bin"', names)
        self.assertIn("link deps/app", names)
        critical = [e for e in trace["traceEvents"] if e.get("cat") == "critical"]
        self.assertEqual(len(critical), 4)


class Inputs(unittest.TestCase):
    def test_parses_link_tracer_lines(self):
        links = pb.parse_links("100.5 100.75 /t/debug/deps/rg-abc\n", 100.0)
        self.assertEqual(links, [{"start": 0.5, "end": 0.75, "duration": 0.25,
                                  "output": "deps/rg-abc"}])

    def test_summarizes_the_wrapper_trace(self):
        line = ('{{"name":"{n}","cat":"wrapper","ph":"X","ts":1,"dur":{d},"pid":7,"tid":0,'
                '"args":{{"crate":"syn","outcome":"miss"}}}}')
        text = "\n".join(line.format(n=n, d=d) for n, d in
                         (("invocation", 50_000), ("pre_key", 1_000), ("rustc", 45_000),
                          ("ingest", 4_000)))
        summary = pb.wrapper_summary(pb.parse_wrapper_trace(text))
        self.assertEqual(summary["outcomes"], {"miss": 1})
        self.assertAlmostEqual(summary["overhead_total"], 0.005)
        self.assertEqual(summary["steps"]["rustc"]["count"], 1)

    def test_reads_measureme_summary_tables(self):
        table = ("+------+------+\n"
                 "| Item | Self time | % of total time | Time |\n"
                 "+------+------+\n"
                 "| codegen_copy_artifacts_from_incr_cache ....  | 868.36ms  | 62.677 | 1s |\n"
                 "| run_linker ....  | 83.95ms  | 6.059 | 1s |\n")
        self.assertEqual(pb.summarize_rows(table),
                         [("codegen_copy_artifacts_from_incr_cache", "868.36ms", "62.677"),
                          ("run_linker", "83.95ms", "6.059")])


if __name__ == "__main__":
    unittest.main()
