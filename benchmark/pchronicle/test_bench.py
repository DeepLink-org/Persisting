import importlib.util
import json
import pathlib
import tempfile
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("bench.py")
SPEC = importlib.util.spec_from_file_location("pchronicle_bench", MODULE_PATH)
bench = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(bench)


def report(commit: str, value: float) -> dict:
    data = {
        "schema": bench.SCHEMA,
        "run": {
            "git_commit": commit,
            "suite": "smoke",
            "os": "linux",
            "arch": "x86_64",
            "cpu": "fixture",
            "logical_cpus": 4,
            "recorded_at": "2026-08-16T00:00:00+00:00",
        },
        "configuration": {},
        "measurements": {},
    }
    bench.store_metrics(
        data,
        [
            bench.metric(
                "catalog/point",
                "latency_p95_ms",
                value,
                "ms",
                "lower",
                "fixture",
            )
        ],
    )
    return data


class BenchmarkReportTests(unittest.TestCase):
    def test_comparison_classifies_directional_regression(self) -> None:
        rows = bench.comparisons(report("candidate", 130), report("main", 100), 0.2)
        self.assertEqual(rows[0]["status"], "regression")
        self.assertAlmostEqual(rows[0]["delta"], 0.3)

    def test_bencher_projection_omits_neutral_metrics(self) -> None:
        data = report("candidate", 100)
        bench.store_metrics(
            data,
            [bench.metric("storage", "ratio", 1.2, "ratio", "neutral", "fixture")],
        )
        projected = bench.bencher_projection(data)
        self.assertIn("catalog/point", projected)
        self.assertNotIn("storage", projected)

    def test_result_parser_does_not_duplicate_scenario_dimension(self) -> None:
        metrics = bench.parse_result_lines(
            "RESULT benchmark=json_streaming shape=ndjson rows=10 p95_ms=2.5",
            "json_streaming_ndjson",
        )
        self.assertEqual(metrics[0]["case"], "system/json_streaming_ndjson/json_streaming")
        self.assertEqual(metrics[0]["metric"], "p95_ms")

    def test_jsonpath_inserts_and_reads_nested_measurement(self) -> None:
        data = {"measurements": {}}
        path = bench.metric_jsonpath("projection/build", "latency_ms")
        value = {"value": 12.5, "unit": "ms", "direction": "lower", "source": "fixture"}
        bench.jsonpath_set(data, path, value)
        self.assertEqual(bench.jsonpath_get(data, path), value)
        self.assertEqual(bench.measurement_at(data, path)["case"], "projection/build")
        with self.assertRaises(ValueError):
            bench.jsonpath_set(data, path, value)

    def test_report_can_be_loaded_from_jsonpath_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / "nightly.json"
            document = {"latest": None}
            bench.jsonpath_set(document, '$["latest"]', report("candidate", 10), replace=True)
            path.write_text(json.dumps(document), encoding="utf-8")
            loaded = bench.read_report(path, '$["latest"]')
            self.assertEqual(loaded["run"]["git_commit"], "candidate")

    def test_readme_update_changes_only_generated_block(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            report_path = root / "report.json"
            readme = root / "README.md"
            report_path.write_text(json.dumps(report("candidate", 12.5)), encoding="utf-8")
            readme.write_text(
                f"before\n{bench.README_START}\nold\n{bench.README_END}\nafter\n",
                encoding="utf-8",
            )
            args = type(
                "Args",
                (),
                {"report": str(report_path), "readme": str(readme), "report_url": None},
            )()
            bench.update_readme(args)
            updated = readme.read_text(encoding="utf-8")
            self.assertTrue(updated.startswith("before\n"))
            self.assertTrue(updated.endswith("\nafter\n"))
            self.assertIn("candidate", updated)

    def test_html_report_renders_comparison_table(self) -> None:
        rows = bench.comparisons(report("candidate", 130), report("main", 100), 0.2)
        comparison = {
            "baseline_commit": "main",
            "regressions": rows,
            "improvements": [],
        }
        rendered = bench.html_report(
            "# report", comparison, bench.measurement_items(report("candidate", 130))
        )
        self.assertIn("<table>", rendered)
        self.assertIn("JSONPath", rendered)
        self.assertIn("&quot;catalog&quot;", rendered)


if __name__ == "__main__":
    unittest.main()
