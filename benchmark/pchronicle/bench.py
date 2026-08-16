#!/usr/bin/env python3
"""Run, compare, and render the pChronicle benchmark suite.

Criterion owns CPU microbenchmarks. Hyperfine owns process-level repetitions
of the existing storage/query harnesses. This script preserves their raw data
and projects both into one stable report used by CI and nightly documentation.
"""

from __future__ import annotations

import argparse
import datetime as dt
import html
import json
import os
import pathlib
import platform
import re
import shlex
import shutil
import subprocess
import sys
from typing import Any


SCHEMA = "pchronicle-benchmark"
README_START = "<!-- pchronicle-benchmark:start -->"
README_END = "<!-- pchronicle-benchmark:end -->"
RESULT_RE = re.compile(r"^RESULT\s+(?P<fields>.+)$")
JSONPATH_TOKEN_RE = re.compile(r'\[(?P<key>"(?:\\.|[^"\\])*")\]')

SUITES = {
    "smoke": {
        "criterion_samples": 10,
        "criterion_measurement_ms": 250,
        "criterion_warmup_ms": 100,
        "hyperfine_runs": 3,
        "scale": 1,
        "iterations": 3,
    },
    "nightly": {
        "criterion_samples": 30,
        "criterion_measurement_ms": 3_000,
        "criterion_warmup_ms": 1_000,
        "hyperfine_runs": 7,
        "scale": 32,
        "iterations": 10,
    },
}


def run_command(
    command: list[str],
    *,
    cwd: pathlib.Path,
    env: dict[str, str] | None = None,
    log: pathlib.Path | None = None,
) -> subprocess.CompletedProcess[str]:
    print(f"==> {shlex.join(command)}", flush=True)
    completed = subprocess.run(
        command,
        cwd=cwd,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    if log is not None:
        log.parent.mkdir(parents=True, exist_ok=True)
        log.write_text(completed.stdout, encoding="utf-8")
    if completed.returncode != 0:
        sys.stderr.write(completed.stdout)
        raise RuntimeError(
            f"command failed with exit code {completed.returncode}: {shlex.join(command)}"
        )
    return completed


def command_output(command: list[str], cwd: pathlib.Path) -> str:
    return run_command(command, cwd=cwd).stdout.strip()


def environment(repo: pathlib.Path, suite: str) -> dict[str, Any]:
    dirty = subprocess.run(
        ["git", "diff", "--quiet", "--ignore-submodules", "--"], cwd=repo
    ).returncode != 0
    return {
        "suite": suite,
        "git_commit": command_output(["git", "rev-parse", "HEAD"], repo),
        "git_dirty": dirty,
        "recorded_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "os": platform.system().lower(),
        "os_release": platform.release(),
        "arch": platform.machine(),
        "cpu": cpu_name(),
        "logical_cpus": os.cpu_count(),
        "rustc": command_output(["rustc", "--version"], repo),
        "cargo": command_output(["cargo", "--version"], repo),
        "hyperfine": command_output(["hyperfine", "--version"], repo),
    }


def cpu_name() -> str:
    if pathlib.Path("/proc/cpuinfo").is_file():
        for line in pathlib.Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
            if line.lower().startswith("model name"):
                return line.split(":", 1)[1].strip()
    return platform.processor() or "unknown"


def metric(
    case: str,
    name: str,
    value: float,
    unit: str,
    direction: str,
    source: str,
    lower: float | None = None,
    upper: float | None = None,
) -> dict[str, Any]:
    result: dict[str, Any] = {
        "case": case,
        "metric": name,
        "value": value,
        "unit": unit,
        "direction": direction,
        "source": source,
    }
    if lower is not None:
        result["lower"] = lower
    if upper is not None:
        result["upper"] = upper
    return result


def metric_jsonpath(case: str, name: str) -> str:
    segments = ["measurements", *case.split("/"), name]
    if any(not segment for segment in segments):
        raise ValueError(f"benchmark metric has an empty JSONPath segment: {case}/{name}")
    return "$" + "".join(f"[{json.dumps(segment)}]" for segment in segments)


def jsonpath_segments(path: str) -> list[str]:
    if not path.startswith("$"):
        raise ValueError(f"JSONPath must start with '$': {path}")
    segments = []
    position = 1
    while position < len(path):
        matched = JSONPATH_TOKEN_RE.match(path, position)
        if matched is None:
            raise ValueError(
                "benchmark JSONPath supports only bracketed name selectors: " + path
            )
        key = json.loads(matched.group("key"))
        if not isinstance(key, str) or not key:
            raise ValueError(f"JSONPath name selector must be a non-empty string: {path}")
        segments.append(key)
        position = matched.end()
    return segments


def jsonpath_get(document: dict[str, Any], path: str) -> Any:
    current: Any = document
    for segment in jsonpath_segments(path):
        if not isinstance(current, dict) or segment not in current:
            raise KeyError(path)
        current = current[segment]
    return current


def jsonpath_set(
    document: dict[str, Any], path: str, value: Any, *, replace: bool = False
) -> None:
    segments = jsonpath_segments(path)
    if not segments:
        raise ValueError("the benchmark JSONPath root cannot be replaced")
    current = document
    for segment in segments[:-1]:
        child = current.setdefault(segment, {})
        if not isinstance(child, dict):
            raise ValueError(f"JSONPath traverses a non-object value: {path}")
        current = child
    leaf = segments[-1]
    if leaf in current and not replace:
        raise ValueError(f"benchmark metric already exists at {path}")
    current[leaf] = value


def store_metrics(document: dict[str, Any], metrics: list[dict[str, Any]]) -> None:
    for item in metrics:
        path = metric_jsonpath(item["case"], item["metric"])
        measurement = {
            key: value for key, value in item.items() if key not in {"case", "metric"}
        }
        jsonpath_set(document, path, measurement)


def measurement_at(document: dict[str, Any], path: str) -> dict[str, Any]:
    segments = jsonpath_segments(path)
    if len(segments) < 3 or segments[0] != "measurements":
        raise ValueError(f"benchmark measurement JSONPath must start at measurements: {path}")
    stored = jsonpath_get(document, path)
    if not isinstance(stored, dict):
        raise ValueError(f"benchmark measurement is not an object: {path}")
    return {
        "case": "/".join(segments[1:-1]),
        "metric": segments[-1],
        "jsonpath": path,
        **stored,
    }


def measurement_items(document: dict[str, Any]) -> list[dict[str, Any]]:
    measurements = document.get("measurements")
    if not isinstance(measurements, dict):
        raise RuntimeError("benchmark report has no measurement object")
    paths: list[tuple[list[str], str]] = []

    def discover(node: Any, segments: list[str]) -> None:
        if isinstance(node, dict) and {"value", "unit", "direction", "source"} <= node.keys():
            if len(segments) < 2:
                raise RuntimeError("benchmark measurement JSONPath has no case and metric")
            paths.append((segments, metric_jsonpath("/".join(segments[:-1]), segments[-1])))
            return
        if not isinstance(node, dict):
            raise RuntimeError("benchmark measurement tree contains a non-measurement leaf")
        for key in sorted(node):
            discover(node[key], [*segments, key])

    discover(measurements, [])
    items = []
    for _, path in paths:
        items.append(measurement_at(document, path))
    return items


def run_criterion(
    repo: pathlib.Path,
    output: pathlib.Path,
    target_dir: pathlib.Path,
    config: dict[str, int],
) -> list[dict[str, Any]]:
    criterion_root = target_dir / "criterion"
    if criterion_root.exists():
        shutil.rmtree(criterion_root)
    env = os.environ.copy()
    env.update(
        {
            "CARGO_TARGET_DIR": str(target_dir),
            "PCHRONICLE_CRITERION_SAMPLES": str(config["criterion_samples"]),
            "PCHRONICLE_CRITERION_MEASUREMENT_MS": str(
                config["criterion_measurement_ms"]
            ),
            "PCHRONICLE_CRITERION_WARMUP_MS": str(config["criterion_warmup_ms"]),
        }
    )
    run_command(
        [
            "cargo",
            "bench",
            "-p",
            "persisting-pchronicle",
            "--bench",
            "pchronicle_criterion",
            "--no-default-features",
            "--features",
            "lance-store",
            "--locked",
        ],
        cwd=repo,
        env=env,
        log=output / "logs" / "criterion.log",
    )
    metrics: list[dict[str, Any]] = []
    for estimate_path in sorted(criterion_root.glob("**/new/estimates.json")):
        relative = estimate_path.relative_to(criterion_root)
        case = "/".join(relative.parts[:-2])
        estimates = json.loads(estimate_path.read_text(encoding="utf-8"))
        for estimate_name in ("mean", "median"):
            estimate = estimates.get(estimate_name)
            if not estimate:
                continue
            confidence = estimate.get("confidence_interval", {})
            metrics.append(
                metric(
                    f"criterion/{case}",
                    f"latency_{estimate_name}_ns",
                    float(estimate["point_estimate"]),
                    "ns",
                    "lower",
                    "criterion",
                    float(confidence["lower_bound"])
                    if "lower_bound" in confidence
                    else None,
                    float(confidence["upper_bound"])
                    if "upper_bound" in confidence
                    else None,
                )
            )
    if not metrics:
        raise RuntimeError(f"Criterion produced no estimates under {criterion_root}")
    if criterion_root.exists():
        shutil.copytree(criterion_root, output / "criterion", dirs_exist_ok=True)
    return metrics


def build_bench_executable(
    repo: pathlib.Path,
    target_dir: pathlib.Path,
    name: str,
    output: pathlib.Path,
) -> pathlib.Path:
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target_dir)
    completed = run_command(
        [
            "cargo",
            "bench",
            "-p",
            "persisting-pchronicle",
            "--bench",
            name,
            "--no-default-features",
            "--features",
            "lance-store",
            "--no-run",
            "--message-format=json",
            "--locked",
        ],
        cwd=repo,
        env=env,
        log=output / "logs" / f"build-{name}.log",
    )
    executable: pathlib.Path | None = None
    for line in completed.stdout.splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        target = record.get("target", {})
        if (
            record.get("reason") == "compiler-artifact"
            and target.get("name") == name
            and "bench" in target.get("kind", [])
            and record.get("executable")
        ):
            executable = pathlib.Path(record["executable"])
    if executable is None:
        raise RuntimeError(f"cargo did not report an executable for benchmark {name}")
    return executable


def bench_target_exists(repo: pathlib.Path, name: str) -> bool:
    manifest = repo / "crates" / "persisting-pchronicle" / "Cargo.toml"
    return f'name = "{name}"' in manifest.read_text(encoding="utf-8")


def parse_result_lines(text: str, scenario: str) -> list[dict[str, Any]]:
    metrics: list[dict[str, Any]] = []
    for line in text.splitlines():
        matched = RESULT_RE.match(line.strip())
        if not matched:
            continue
        fields: dict[str, str] = {}
        for token in shlex.split(matched.group("fields")):
            if "=" in token:
                key, value = token.split("=", 1)
                fields[key] = value
        benchmark = fields.pop("benchmark", "result")
        dimensions = []
        for key in ("shape",):
            if key not in fields:
                continue
            value = fields.pop(key)
            if value not in scenario:
                dimensions.append(f"{key}-{value}")
        case = "/".join(["system", scenario, *dimensions, benchmark])
        for name, raw_value in fields.items():
            if name in {"iterations", "documents", "rows"}:
                continue
            try:
                value = float(raw_value)
            except ValueError:
                continue
            unit, direction = metric_semantics(name)
            metrics.append(metric(case, name, value, unit, direction, "custom"))
    return metrics


def metric_semantics(name: str) -> tuple[str, str]:
    if name.endswith("_ms"):
        return "ms", "lower"
    if name.endswith("_qps") or name.endswith("_s") or "speedup" in name:
        return "ops/s" if name.endswith(("_qps", "_s")) else "ratio", "higher"
    if name.endswith("_mib"):
        return "MiB", "lower"
    if name.endswith("_bytes") or "allocations" in name:
        return "bytes" if name.endswith("_bytes") else "count", "lower"
    if name == "lance_over_json":
        return "ratio", "lower"
    if "over_lance_time" in name:
        return "ratio", "neutral"
    return "ratio", "neutral"


def run_system_scenario(
    repo: pathlib.Path,
    output: pathlib.Path,
    executable: pathlib.Path,
    scenario: str,
    variables: dict[str, str],
    hyperfine_runs: int,
) -> list[dict[str, Any]]:
    env = os.environ.copy()
    env.update(variables)
    direct = run_command(
        [str(executable)],
        cwd=repo,
        env=env,
        log=output / "logs" / f"{scenario}.log",
    )
    metrics = parse_result_lines(direct.stdout, scenario)

    hyperfine_path = output / "hyperfine" / f"{scenario}.json"
    hyperfine_path.parent.mkdir(parents=True, exist_ok=True)
    command = shlex.join(
        ["env", *[f"{key}={value}" for key, value in variables.items()], str(executable)]
    )
    run_command(
        [
            "hyperfine",
            "--warmup",
            "1",
            "--runs",
            str(hyperfine_runs),
            "--command-name",
            scenario,
            "--export-json",
            str(hyperfine_path),
            command,
        ],
        cwd=repo,
        log=output / "logs" / f"hyperfine-{scenario}.log",
    )
    result = json.loads(hyperfine_path.read_text(encoding="utf-8"))["results"][0]
    case = f"hyperfine/{scenario}"
    for name in ("mean", "median", "stddev", "min", "max"):
        metrics.append(
            metric(
                case,
                f"wall_{name}_seconds",
                float(result[name]),
                "s",
                "lower" if name in {"mean", "median"} else "neutral",
                "hyperfine",
            )
        )
    return metrics


def run_suite(args: argparse.Namespace) -> None:
    repo = pathlib.Path(args.repo).resolve()
    output = pathlib.Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    target_dir = (
        pathlib.Path(args.target_dir).resolve()
        if args.target_dir
        else repo / "target" / "pchronicle-benchmark-build"
    )
    config = SUITES[args.suite]
    metadata = environment(repo, args.suite)
    metrics = run_criterion(repo, output, target_dir, config)

    lance = build_bench_executable(repo, target_dir, "lance_vs_json", output)
    streaming = build_bench_executable(repo, target_dir, "json_streaming", output)
    common = {
        "PCHRONICLE_BENCH_SCALE": str(config["scale"]),
        "PCHRONICLE_BENCH_ITERS": str(config["iterations"]),
    }
    metrics.extend(
        run_system_scenario(
            repo,
            output,
            lance,
            "lance_vs_json",
            common,
            config["hyperfine_runs"],
        )
    )
    if bench_target_exists(repo, "projection_pipeline"):
        projection = build_bench_executable(repo, target_dir, "projection_pipeline", output)
        metrics.extend(
            run_system_scenario(
                repo,
                output,
                projection,
                "projection_pipeline",
                common,
                config["hyperfine_runs"],
            )
        )
    for shape in ("ndjson", "array"):
        metrics.extend(
            run_system_scenario(
                repo,
                output,
                streaming,
                f"json_streaming_{shape}",
                {**common, "PCHRONICLE_BENCH_JSON_SHAPE": shape},
                config["hyperfine_runs"],
            )
        )

    report: dict[str, Any] = {
        "schema": SCHEMA,
        "run": metadata,
        "configuration": config,
        "measurements": {},
    }
    store_metrics(report, sorted(metrics, key=lambda item: (item["case"], item["metric"])))
    write_json(output / "raw-report.json", report)
    write_json(output / "bencher.json", bencher_projection(report))
    render_report(report, None, output, float(args.regression_threshold))


def bencher_projection(report: dict[str, Any]) -> dict[str, Any]:
    projected: dict[str, Any] = {}
    for item in measurement_items(report):
        if item["direction"] == "neutral":
            continue
        measure = {"value": item["value"]}
        if "lower" in item:
            measure["lower_value"] = item["lower"]
        if "upper" in item:
            measure["upper_value"] = item["upper"]
        projected.setdefault(item["case"], {})[item["metric"]] = measure
    return projected


def compare_reports(args: argparse.Namespace) -> None:
    candidate = read_report(pathlib.Path(args.candidate))
    baseline = read_report(pathlib.Path(args.baseline)) if args.baseline else None
    render_report(
        candidate,
        baseline,
        pathlib.Path(args.output),
        float(args.regression_threshold),
    )


def read_report(path: pathlib.Path, root_jsonpath: str | None = None) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    report = jsonpath_get(document, root_jsonpath) if root_jsonpath else document
    if (
        not isinstance(report, dict)
        or report.get("schema") != SCHEMA
        or not isinstance(report.get("measurements"), dict)
    ):
        raise RuntimeError(f"unsupported benchmark report: {path}")
    return report


def comparisons(
    candidate: dict[str, Any], baseline: dict[str, Any] | None, threshold: float
) -> list[dict[str, Any]]:
    if baseline is None:
        return []
    for field in ("suite", "os", "arch", "cpu"):
        if candidate["run"].get(field) != baseline["run"].get(field):
            raise RuntimeError(
                f"benchmark reports use different {field}: "
                f"{baseline['run'].get(field)!r} != {candidate['run'].get(field)!r}"
            )
    baseline_metrics = {
        (item["case"], item["metric"]): item for item in measurement_items(baseline)
    }
    rows = []
    for current in measurement_items(candidate):
        previous = baseline_metrics.get((current["case"], current["metric"]))
        if not previous or previous["unit"] != current["unit"]:
            continue
        old = float(previous["value"])
        new = float(current["value"])
        delta = None if old == 0 else (new - old) / abs(old)
        direction = current["direction"]
        regression = (
            delta is not None
            and direction != "neutral"
            and ((direction == "lower" and delta > 0) or (direction == "higher" and delta < 0))
        )
        improvement = (
            delta is not None
            and direction != "neutral"
            and ((direction == "lower" and delta < 0) or (direction == "higher" and delta > 0))
        )
        if regression and abs(delta) >= threshold:
            status = "regression"
        elif improvement and abs(delta) >= threshold:
            status = "improvement"
        else:
            status = "stable"
        rows.append(
            {
                "case": current["case"],
                "metric": current["metric"],
                "jsonpath": current["jsonpath"],
                "unit": current["unit"],
                "baseline": old,
                "candidate": new,
                "delta": delta,
                "status": status,
            }
        )
    return rows


def render_report(
    candidate: dict[str, Any],
    baseline: dict[str, Any] | None,
    output: pathlib.Path,
    threshold: float,
) -> None:
    output.mkdir(parents=True, exist_ok=True)
    rows = comparisons(candidate, baseline, threshold)
    regressions = sorted(
        (row for row in rows if row["status"] == "regression"),
        key=lambda row: abs(row["delta"]),
        reverse=True,
    )
    improvements = sorted(
        (row for row in rows if row["status"] == "improvement"),
        key=lambda row: abs(row["delta"]),
        reverse=True,
    )
    comparison = {
        "schema": SCHEMA,
        "candidate_commit": candidate["run"]["git_commit"],
        "baseline_commit": baseline["run"]["git_commit"] if baseline else None,
        "threshold": threshold,
        "regressions": regressions,
        "improvements": improvements,
        "comparisons": rows,
    }
    write_json(output / "comparison.json", comparison)
    markdown = markdown_report(candidate, baseline, regressions, improvements, threshold)
    (output / "report.md").write_text(markdown, encoding="utf-8")
    (output / "report.html").write_text(
        html_report(markdown, comparison, measurement_items(candidate)), encoding="utf-8"
    )


def markdown_report(
    candidate: dict[str, Any],
    baseline: dict[str, Any] | None,
    regressions: list[dict[str, Any]],
    improvements: list[dict[str, Any]],
    threshold: float,
) -> str:
    run = candidate["run"]
    lines = [
        "# pChronicle benchmark report",
        "",
        f"- Candidate: `{run['git_commit']}`",
        f"- Baseline: `{baseline['run']['git_commit']}`" if baseline else "- Baseline: unavailable",
        f"- Suite: `{run['suite']}`",
        f"- Testbed: `{run['os']}/{run['arch']}` · {run['cpu']} · "
        f"{run['logical_cpus']} logical CPUs",
        f"- Regression threshold: `{threshold * 100:.1f}%`",
        "",
    ]
    if baseline:
        lines.extend(render_comparison_section("Regressions", regressions))
        lines.extend(render_comparison_section("Improvements", improvements))
    lines.extend(
        [
            "## Candidate metrics",
            "",
            "| JSONPath | Value | Unit | Source |",
            "|---|---:|---|---|",
        ]
    )
    for item in measurement_items(candidate):
        lines.append(
            f"| `{item['jsonpath']}` | "
            f"{format_number(item['value'])} | {item['unit']} | {item['source']} |"
        )
    lines.append("")
    return "\n".join(lines)


def render_comparison_section(title: str, rows: list[dict[str, Any]]) -> list[str]:
    lines = [f"## {title}", ""]
    if not rows:
        return [*lines, "None above the configured threshold.", ""]
    lines.extend(["| JSONPath | Main | Candidate | Delta |", "|---|---:|---:|---:|"])
    for row in rows:
        lines.append(
            f"| `{row['jsonpath']}` | "
            f"{format_number(row['baseline'])} | {format_number(row['candidate'])} | "
            f"{row['delta']:+.1%} |"
        )
    lines.append("")
    return lines


def html_report(
    markdown: str, comparison: dict[str, Any], metrics: list[dict[str, Any]] | None = None
) -> str:
    regressions = comparison["regressions"]
    improvements = comparison["improvements"]
    if comparison.get("baseline_commit") is None:
        verdict, tone = "NO BASELINE", "neutral"
    elif regressions:
        verdict, tone = "REGRESSION", "bad"
    else:
        verdict, tone = "PASS", "good"
    sections = []
    comparison_sections = (
        (("Regressions", regressions), ("Improvements", improvements))
        if comparison.get("baseline_commit") is not None
        else ()
    )
    for title, rows in comparison_sections:
        body = "".join(
            "<tr>"
            f"<td><code>{html.escape(row['jsonpath'])}</code></td>"
            f"<td>{format_number(row['baseline'])}</td>"
            f"<td>{format_number(row['candidate'])}</td>"
            f"<td>{row['delta']:+.1%}</td>"
            "</tr>"
            for row in rows
        )
        if not body:
            body = '<tr><td colspan="4">None above the configured threshold.</td></tr>'
        sections.append(
            f"<h2>{title}</h2><table><thead><tr><th>JSONPath</th>"
            f"<th>Main</th><th>Candidate</th><th>Delta</th></tr></thead><tbody>{body}</tbody></table>"
        )
    metric_rows = "".join(
        "<tr>"
        f"<td><code>{html.escape(item['jsonpath'])}</code></td>"
        f"<td>{format_number(item['value'])}</td>"
        f"<td>{html.escape(item['unit'])}</td>"
        f"<td>{html.escape(item['source'])}</td>"
        "</tr>"
        for item in metrics or []
    )
    if metric_rows:
        sections.append(
            "<h2>Candidate metrics</h2><table><thead><tr><th>JSONPath</th>"
            f"<th>Value</th><th>Unit</th><th>Source</th></tr></thead><tbody>{metric_rows}</tbody></table>"
        )
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>pChronicle benchmark report</title>
<style>
body{{font:15px/1.5 system-ui,sans-serif;max-width:1100px;margin:40px auto;padding:0 24px;color:#18202a}}
pre{{white-space:pre-wrap;background:#f5f7fa;padding:20px;border-radius:8px}} .good{{color:#08783e}} .bad{{color:#b42318}} .neutral{{color:#59636e}}
.summary{{display:flex;gap:24px;align-items:baseline}} code{{background:#eef1f5;padding:2px 4px;border-radius:3px}}
table{{border-collapse:collapse;width:100%;margin-bottom:28px}} th,td{{padding:8px 10px;border:1px solid #d8dee6;text-align:right}}
th:first-child,td:first-child{{text-align:left}} th{{background:#f5f7fa}}
</style></head><body><div class="summary"><h1>pChronicle benchmark</h1>
<strong class="{tone}">{verdict}</strong><span>{len(regressions)} regressions · {len(improvements)} improvements</span></div>
{''.join(sections)}<details><summary>Complete Markdown report</summary><pre>{html.escape(markdown)}</pre></details></body></html>
"""


def update_readme(args: argparse.Namespace) -> None:
    report = read_report(
        pathlib.Path(args.report), getattr(args, "report_jsonpath", None)
    )
    readme = pathlib.Path(args.readme)
    content = readme.read_text(encoding="utf-8")
    if README_START not in content or README_END not in content:
        raise RuntimeError(f"benchmark markers are missing from {readme}")
    run = report["run"]
    priorities = [
        metric_jsonpath("criterion/atif_conversion/parse_corpus", "latency_median_ns"),
        metric_jsonpath(
            "criterion/atif_conversion/roundtrip_corpus", "latency_median_ns"
        ),
        metric_jsonpath(
            "criterion/projection_cpu/events_to_storyline_corpus", "latency_median_ns"
        ),
        metric_jsonpath("system/projection_pipeline/event_append", "initial_append_ms"),
        metric_jsonpath("system/projection_pipeline/projection_build", "build_ms"),
        metric_jsonpath("system/projection_pipeline/projection_incremental", "sync_ms"),
        metric_jsonpath("system/lance_vs_json/lifecycle", "cold_query_ms"),
        metric_jsonpath("system/lance_vs_json/lifecycle", "get_storyline_full_ms"),
        metric_jsonpath("system/lance_vs_json/lifecycle", "replace_storyline_ms"),
        metric_jsonpath("system/lance_vs_json/selective", "lance_qps"),
        metric_jsonpath("system/lance_vs_json/group_by", "lance_qps"),
        metric_jsonpath("system/lance_vs_json/summary", "lance_over_json"),
        metric_jsonpath("system/json_streaming_ndjson/json_streaming", "p95_ms"),
        metric_jsonpath("system/json_streaming_ndjson/json_streaming", "rows_s"),
        metric_jsonpath(
            "system/json_streaming_ndjson/json_streaming", "process_peak_rss_mib"
        ),
        metric_jsonpath("hyperfine/projection_pipeline", "wall_median_seconds"),
        metric_jsonpath("hyperfine/lance_vs_json", "wall_median_seconds"),
    ]
    selected = []
    for path in priorities:
        try:
            selected.append(measurement_at(report, path))
        except KeyError:
            continue
    lines = [
        README_START,
        f"Latest nightly pChronicle benchmark: `{run['git_commit'][:12]}` on "
        f"`{run['os']}/{run['arch']}` ({run['recorded_at']}).",
        "",
        "| Case | Metric | Value |",
        "|---|---:|---:|",
    ]
    for item in selected:
        lines.append(
            f"| `{item['case']}` | `{item['metric']}` | "
            f"{format_number(item['value'])} {item['unit']} |"
        )
    if args.report_url:
        lines.extend(["", f"[Open the complete benchmark run]({args.report_url})."])
    lines.append(README_END)
    replacement = "\n".join(lines)
    prefix, rest = content.split(README_START, 1)
    _, suffix = rest.split(README_END, 1)
    readme.write_text(prefix + replacement + suffix, encoding="utf-8")


def jsonpath_get_command(args: argparse.Namespace) -> None:
    document = json.loads(pathlib.Path(args.document).read_text(encoding="utf-8"))
    print(json.dumps(jsonpath_get(document, args.path), indent=2, sort_keys=True))


def jsonpath_set_command(args: argparse.Namespace) -> None:
    document_path = pathlib.Path(args.document)
    document = json.loads(document_path.read_text(encoding="utf-8"))
    value = (
        json.loads(pathlib.Path(args.value_file).read_text(encoding="utf-8"))
        if args.value_file
        else json.loads(args.value_json)
    )
    jsonpath_set(document, args.path, value, replace=args.replace)
    write_json(document_path, document)


def format_number(value: float) -> str:
    value = float(value)
    if value == 0:
        return "0"
    if abs(value) >= 10_000 or abs(value) < 0.001:
        return f"{value:.4g}"
    return f"{value:.3f}".rstrip("0").rstrip(".")


def write_json(path: pathlib.Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="run Criterion and hyperfine suites")
    run.add_argument("--repo", default=".")
    run.add_argument("--output", required=True)
    run.add_argument("--target-dir")
    run.add_argument("--suite", choices=sorted(SUITES), default="smoke")
    run.add_argument("--regression-threshold", type=float, default=0.30)
    run.set_defaults(handler=run_suite)

    compare = commands.add_parser("compare", help="compare and render benchmark reports")
    compare.add_argument("--candidate", required=True)
    compare.add_argument("--baseline")
    compare.add_argument("--output", required=True)
    compare.add_argument("--regression-threshold", type=float, default=0.30)
    compare.set_defaults(handler=compare_reports)

    update = commands.add_parser("update-readme", help="update the generated README section")
    update.add_argument("--report", required=True)
    update.add_argument("--readme", required=True)
    update.add_argument("--report-url")
    update.add_argument("--report-jsonpath")
    update.set_defaults(handler=update_readme)

    get = commands.add_parser("jsonpath-get", help="read one JSON value by JSONPath")
    get.add_argument("--document", required=True)
    get.add_argument("--path", required=True)
    get.set_defaults(handler=jsonpath_get_command)

    set_value = commands.add_parser("jsonpath-set", help="insert one JSON value by JSONPath")
    set_value.add_argument("--document", required=True)
    set_value.add_argument("--path", required=True)
    value = set_value.add_mutually_exclusive_group(required=True)
    value.add_argument("--value-json")
    value.add_argument("--value-file")
    set_value.add_argument("--replace", action="store_true")
    set_value.set_defaults(handler=jsonpath_set_command)
    return root


def main() -> int:
    args = parser().parse_args()
    args.handler(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
