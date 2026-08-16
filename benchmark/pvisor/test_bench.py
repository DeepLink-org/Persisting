#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import pathlib
import unittest

MODULE_PATH = pathlib.Path(__file__).with_name("bench.py")
SPEC = importlib.util.spec_from_file_location("pvisor_bench", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)


def report(value: float) -> dict:
    return {
        "schema": BENCH.SCHEMA,
        "environment": {
            "git_commit": "commit",
            "suite": "smoke",
            "os": "linux",
            "arch": "x86_64",
            "cpu": "test-cpu",
        },
        "measurements": {
            "process": {
                "host_run": {
                    "latency_ms_p50": BENCH.measurement(value, "ms", "lower", "wall-clock")
                }
            }
        },
    }


class PVisorBenchmarkTests(unittest.TestCase):
    def test_percentile_interpolates(self) -> None:
        self.assertEqual(BENCH.percentile([1.0, 2.0, 3.0, 4.0], 50), 2.5)
        self.assertAlmostEqual(BENCH.percentile([1.0, 2.0], 95), 1.95)

    def test_comparison_classifies_lower_is_better(self) -> None:
        comparison = BENCH.compare_reports(report(120), report(100), 15)
        self.assertEqual(comparison["regressions"], 1)
        self.assertEqual(comparison["metrics"][0]["status"], "regression")

    def test_candidate_only_report_is_supported(self) -> None:
        comparison = BENCH.compare_reports(report(100), None, 15)
        self.assertEqual(comparison["regressions"], 0)
        self.assertEqual(comparison["metrics"][0]["status"], "candidate-only")

    def test_different_suites_cannot_be_compared(self) -> None:
        candidate = report(100)
        baseline = report(100)
        baseline["environment"]["suite"] = "nightly"
        with self.assertRaisesRegex(ValueError, "environment.suite"):
            BENCH.compare_reports(candidate, baseline, 15)


if __name__ == "__main__":
    unittest.main()
