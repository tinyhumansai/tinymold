"""Tests for the cache server benchmark's helpers and its report table.

Run with: python3 -m unittest discover -s bench -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import report  # noqa: E402
import server  # noqa: E402

MACHINE = {"os": "darwin", "arch": "arm64", "cpus": 8, "rustc": "rustc 1.99.0"}


class Percentile(unittest.TestCase):
    def test_the_median_and_tail_are_taken_by_nearest_rank(self):
        samples = [float(n) for n in range(1, 101)]
        self.assertEqual(server.percentile(samples, 0.5), 50.0)
        self.assertEqual(server.percentile(samples, 0.99), 99.0)

    def test_tiny_and_unordered_samples_stay_in_range(self):
        self.assertEqual(server.percentile([3.0, 1.0, 2.0], 0.5), 2.0)
        self.assertEqual(server.percentile([7.0], 0.99), 7.0)


class ServerSection(unittest.TestCase):
    def test_each_case_is_a_row_with_a_column_per_server(self):
        result = {
            "benchmark": "server", "machine": MACHINE, "date": "2026-10-15", "runs": 5,
            "servers": {"tinymold serve": "release", "bazel-remote": "native"},
            "requests": 2000, "entries": 20000, "not_measured": ["FindMissingBlobs (gRPC)"],
            "results": [
                {"server": "tinymold serve", "case": "hit 1 KiB p50 (ms)", "median": 0.16},
                {"server": "bazel-remote", "case": "hit 1 KiB p50 (ms)", "median": 0.14},
                {"server": "tinymold serve", "case": "cold start (s)", "median": 0.017},
            ],
        }
        lines = report.server_section(result)
        self.assertIn("| Case | tinymold serve | bazel-remote |", lines)
        self.assertIn("| hit 1 KiB p50 (ms) | 0.16 | 0.14 |", lines)
        self.assertIn("| cold start (s) | 0.017 | – |", lines)
        self.assertTrue(any("FindMissingBlobs (gRPC)" in line for line in lines))


if __name__ == "__main__":
    unittest.main()
