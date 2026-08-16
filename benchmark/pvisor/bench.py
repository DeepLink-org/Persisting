#!/usr/bin/env python3
"""Run and compare the pVisor process-level benchmark suite."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import pathlib
import platform
import shlex
import statistics
import subprocess
import sys
import tempfile
import time
from typing import Any

SCHEMA = "pvisor-benchmark/v1"
SUITES = {
    "smoke": {"warmups": 2, "samples": 10},
    "nightly": {"warmups": 10, "samples": 50},
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


def resolve_path(value: str, repo: pathlib.Path) -> pathlib.Path:
    path = pathlib.Path(value)
    return path.resolve() if path.is_absolute() else (repo / path).resolve()


def cpu_name() -> str:
    cpuinfo = pathlib.Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        for line in cpuinfo.read_text(encoding="utf-8").splitlines():
            if line.lower().startswith("model name"):
                return line.split(":", 1)[1].strip()
    return platform.processor() or "unknown"


def environment(repo: pathlib.Path, suite: str) -> dict[str, Any]:
    dirty = bool(command_output(["git", "status", "--porcelain"], repo))
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
        "python": platform.python_version(),
    }


def percentile(values: list[float], percent: float) -> float:
    if not values:
        raise ValueError("cannot calculate a percentile without samples")
    ordered = sorted(values)
    position = (len(ordered) - 1) * percent / 100
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    weight = position - lower
    return ordered[lower] * (1 - weight) + ordered[upper] * weight


def distribution(values: list[float]) -> dict[str, Any]:
    return {
        "samples": len(values),
        "raw_ms": [round(value, 6) for value in values],
        "min_ms": round(min(values), 6),
        "mean_ms": round(statistics.fmean(values), 6),
        "p50_ms": round(percentile(values, 50), 6),
        "p95_ms": round(percentile(values, 95), 6),
        "p99_ms": round(percentile(values, 99), 6),
        "max_ms": round(max(values), 6),
        "stdev_ms": round(statistics.pstdev(values), 6),
    }


def measurement(value: float, unit: str, direction: str, source: str) -> dict[str, Any]:
    return {
        "value": round(value, 6),
        "unit": unit,
        "direction": direction,
        "source": source,
    }


def timed_command(
    command: list[str],
    *,
    cwd: pathlib.Path,
    env: dict[str, str],
    capture_json: bool = False,
) -> tuple[float, dict[str, Any] | None]:
    stdout: int | None = subprocess.PIPE if capture_json else subprocess.DEVNULL
    started = time.perf_counter_ns()
    completed = subprocess.run(
        command,
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=stdout,
        stderr=subprocess.PIPE,
        check=False,
    )
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    if completed.returncode != 0:
        stderr = completed.stderr.decode(errors="replace") if completed.stderr else ""
        raise RuntimeError(
            f"benchmark command failed ({completed.returncode}): {shlex.join(command)}\n{stderr}"
        )
    document = None
    if capture_json:
        try:
            document = json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            raise RuntimeError(
                f"benchmark command did not emit JSON: {shlex.join(command)}"
            ) from error
    return elapsed_ms, document


def run_direct(binary: pathlib.Path, cwd: pathlib.Path, env: dict[str, str]) -> float:
    elapsed, _ = timed_command([str(binary)], cwd=cwd, env=env)
    return elapsed


def run_pvisor(
    pvisor: pathlib.Path,
    cwd: pathlib.Path,
    run_home: pathlib.Path,
    env: dict[str, str],
) -> tuple[float, pathlib.Path]:
    before = set(run_home.glob("run-*")) if run_home.exists() else set()
    elapsed, _ = timed_command(
        [str(pvisor), "run", "--stdio", "capture", "--", "/usr/bin/true"],
        cwd=cwd,
        env=env,
    )
    created = sorted(set(run_home.glob("run-*")) - before)
    if len(created) != 1:
        raise RuntimeError(f"expected one pVisor Run, found {len(created)}")
    validate_bundle(created[0] / "run-bundle.json")
    return elapsed, created[0]


def validate_bundle(path: pathlib.Path) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    run = document.get("run", {})
    if run.get("state") != "completed" or run.get("exit_code") != 0:
        raise RuntimeError(f"pVisor benchmark produced an invalid Run Bundle: {path}")
    return document


def sample_json_command(
    command: list[str],
    *,
    cwd: pathlib.Path,
    env: dict[str, str],
    expected_state: str,
) -> float:
    elapsed, document = timed_command(command, cwd=cwd, env=env, capture_json=True)
    assert document is not None
    if document.get("run", {}).get("state") != expected_state:
        raise RuntimeError(f"unexpected Run state from {shlex.join(command)}")
    return elapsed


def benchmark(
    repo: pathlib.Path,
    output: pathlib.Path,
    target_dir: pathlib.Path,
    suite: str,
) -> dict[str, Any]:
    config = SUITES[suite]
    output.mkdir(parents=True, exist_ok=True)
    target_dir.mkdir(parents=True, exist_ok=True)
    build_env = os.environ.copy()
    build_env["CARGO_TARGET_DIR"] = str(target_dir)
    run_command(
        [
            "cargo",
            "build",
            "--release",
            "--locked",
            "-p",
            "persisting-pvisor",
            "--bin",
            "pvisor",
        ],
        cwd=repo,
        env=build_env,
        log=output / "logs" / "build.log",
    )
    pvisor = target_dir / "release" / "pvisor"
    direct_binary = pathlib.Path("/usr/bin/true")
    if not pvisor.is_file() or not direct_binary.is_file():
        raise RuntimeError("benchmark binaries are unavailable")

    with tempfile.TemporaryDirectory(prefix="pvisor-benchmark-") as temporary:
        work = pathlib.Path(temporary)
        workspace = work / "workspace"
        run_home = work / "runs"
        workspace.mkdir()
        run_env = os.environ.copy()
        run_env["PERSISTING_RUN_HOME"] = str(run_home)

        last_run: pathlib.Path | None = None
        for _ in range(config["warmups"]):
            run_direct(direct_binary, workspace, run_env)
            _, last_run = run_pvisor(pvisor, workspace, run_home, run_env)

        direct_samples: list[float] = []
        pvisor_samples: list[float] = []
        for _ in range(config["samples"]):
            direct_samples.append(run_direct(direct_binary, workspace, run_env))
            elapsed, last_run = run_pvisor(pvisor, workspace, run_home, run_env)
            pvisor_samples.append(elapsed)

        assert last_run is not None
        status_command = [str(pvisor), "status", "--json", str(last_run)]
        review_command = [str(pvisor), "review", "--json", str(last_run)]
        sample_json_command(
            status_command,
            cwd=workspace,
            env=run_env,
            expected_state="completed",
        )
        sample_json_command(
            review_command,
            cwd=workspace,
            env=run_env,
            expected_state="completed",
        )
        status_samples = [
            sample_json_command(
                status_command,
                cwd=workspace,
                env=run_env,
                expected_state="completed",
            )
            for _ in range(config["samples"])
        ]
        review_samples = [
            sample_json_command(
                review_command,
                cwd=workspace,
                env=run_env,
                expected_state="completed",
            )
            for _ in range(config["samples"])
        ]
        bundle_bytes = (last_run / "run-bundle.json").stat().st_size

    distributions = {
        "direct": distribution(direct_samples),
        "host_run": distribution(pvisor_samples),
        "status": distribution(status_samples),
        "review": distribution(review_samples),
    }
    direct_p50 = distributions["direct"]["p50_ms"]
    pvisor_p50 = distributions["host_run"]["p50_ms"]
    overhead = max(0.0, pvisor_p50 - direct_p50)
    ratio = pvisor_p50 / direct_p50 if direct_p50 else 0.0
    document = {
        "schema": SCHEMA,
        "environment": environment(repo, suite),
        "suite_config": config,
        "measurements": {
            "process": {
                "direct": {
                    "latency_ms_p50": measurement(direct_p50, "ms", "lower", "wall-clock"),
                    "latency_ms_p95": measurement(
                        distributions["direct"]["p95_ms"],
                        "ms",
                        "lower",
                        "wall-clock",
                    ),
                },
                "host_run": {
                    "latency_ms_p50": measurement(pvisor_p50, "ms", "lower", "wall-clock"),
                    "latency_ms_p95": measurement(
                        distributions["host_run"]["p95_ms"],
                        "ms",
                        "lower",
                        "wall-clock",
                    ),
                    "overhead_ms_p50": measurement(overhead, "ms", "lower", "derived"),
                    "overhead_ratio_p50": measurement(ratio, "x", "lower", "derived"),
                },
            },
            "bundle": {
                "status": {
                    "latency_ms_p50": measurement(
                        distributions["status"]["p50_ms"],
                        "ms",
                        "lower",
                        "wall-clock",
                    )
                },
                "review": {
                    "latency_ms_p50": measurement(
                        distributions["review"]["p50_ms"],
                        "ms",
                        "lower",
                        "wall-clock",
                    )
                },
                "run_bundle_bytes": measurement(
                    float(bundle_bytes), "bytes", "lower", "filesystem"
                ),
            },
        },
        "distributions": distributions,
    }
    return document


def measurement_items(document: dict[str, Any]) -> dict[str, dict[str, Any]]:
    found: dict[str, dict[str, Any]] = {}

    def visit(node: Any, segments: list[str]) -> None:
        if isinstance(node, dict) and {"value", "unit", "direction", "source"} <= set(node):
            found["/".join(segments)] = node
            return
        if not isinstance(node, dict):
            raise ValueError("measurement tree contains a non-measurement leaf")
        for key in sorted(node):
            visit(node[key], [*segments, key])

    visit(document["measurements"], [])
    return found


def render_run_report(document: dict[str, Any]) -> str:
    environment_data = document["environment"]
    lines = [
        "# pVisor benchmark",
        "",
        f"- Suite: `{environment_data['suite']}`",
        f"- Commit: `{environment_data['git_commit']}`",
        f"- Host: `{environment_data['os']} {environment_data['arch']}` / "
        f"`{environment_data['cpu']}`",
        f"- Samples: `{document['suite_config']['samples']}` after "
        f"`{document['suite_config']['warmups']}` warmups",
        "",
        "| Metric | Value | Unit |",
        "|---|---:|---|",
    ]
    for path, item in measurement_items(document).items():
        lines.append(f"| `{path}` | {item['value']:.6g} | {item['unit']} |")
    lines.extend(
        [
            "",
            "Every measured `pvisor run` must exit successfully and produce a "
            "completed, zero-exit-code Run Bundle.",
            "",
        ]
    )
    return "\n".join(lines)


def compare_reports(
    candidate: dict[str, Any],
    baseline: dict[str, Any] | None,
    threshold: float,
) -> dict[str, Any]:
    if baseline is not None:
        baseline_environment = baseline["environment"]
        candidate_environment = candidate["environment"]
        for key in ("suite", "os", "arch", "cpu"):
            if baseline_environment.get(key) != candidate_environment.get(key):
                raise ValueError(
                    f"cannot compare reports with different environment.{key}: "
                    f"{baseline_environment.get(key)!r} != "
                    f"{candidate_environment.get(key)!r}"
                )
    candidate_items = measurement_items(candidate)
    baseline_items = measurement_items(baseline) if baseline is not None else {}
    rows = []
    regressions = 0
    for path, current in candidate_items.items():
        previous = baseline_items.get(path)
        delta_pct = None
        status = "candidate-only"
        if previous is not None:
            if previous["unit"] != current["unit"]:
                raise ValueError(f"unit changed for {path}")
            baseline_value = float(previous["value"])
            candidate_value = float(current["value"])
            if baseline_value != 0:
                delta_pct = (candidate_value - baseline_value) / baseline_value * 100
                signed = delta_pct if current["direction"] == "lower" else -delta_pct
                if signed > threshold:
                    status = "regression"
                    regressions += 1
                elif signed < -threshold:
                    status = "improvement"
                else:
                    status = "stable"
            else:
                status = "stable" if candidate_value == 0 else "changed"
        rows.append(
            {
                "path": path,
                "baseline": previous["value"] if previous is not None else None,
                "candidate": current["value"],
                "unit": current["unit"],
                "delta_pct": round(delta_pct, 3) if delta_pct is not None else None,
                "status": status,
            }
        )
    return {
        "schema": "pvisor-benchmark-comparison/v1",
        "threshold_pct": threshold,
        "regressions": regressions,
        "baseline_commit": baseline["environment"]["git_commit"] if baseline is not None else None,
        "candidate_commit": candidate["environment"]["git_commit"],
        "metrics": rows,
    }


def render_comparison(document: dict[str, Any]) -> str:
    lines = [
        "# pVisor benchmark comparison",
        "",
        f"Regression threshold: `{document['threshold_pct']:.3g}%`; "
        f"regressions: `{document['regressions']}`.",
        "",
        "| Metric | Baseline | Candidate | Delta | Status |",
        "|---|---:|---:|---:|---|",
    ]
    for row in document["metrics"]:
        baseline = "—" if row["baseline"] is None else f"{row['baseline']:.6g}"
        delta = "—" if row["delta_pct"] is None else f"{row['delta_pct']:+.2f}%"
        lines.append(
            f"| `{row['path']}` | {baseline} | {row['candidate']:.6g} "
            f"{row['unit']} | {delta} | {row['status']} |"
        )
    lines.append("")
    return "\n".join(lines)


def load_report(path: pathlib.Path) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("schema") != SCHEMA:
        raise ValueError(f"unsupported pVisor benchmark schema in {path}")
    measurement_items(document)
    return document


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    run_parser = subparsers.add_parser("run")
    run_parser.add_argument("--repo", default=str(pathlib.Path(__file__).parents[2]))
    run_parser.add_argument("--suite", choices=sorted(SUITES), default="smoke")
    run_parser.add_argument("--output", required=True)
    run_parser.add_argument("--target-dir", default="target/pvisor-benchmark-build")

    compare_parser = subparsers.add_parser("compare")
    compare_parser.add_argument("--repo", default=str(pathlib.Path(__file__).parents[2]))
    compare_parser.add_argument("--candidate", required=True)
    compare_parser.add_argument("--baseline", default="")
    compare_parser.add_argument("--output", required=True)
    compare_parser.add_argument("--regression-threshold", type=float, default=15.0)
    compare_parser.add_argument("--fail-on-regression", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    repo = pathlib.Path(args.repo).resolve()
    output = resolve_path(args.output, repo)
    output.mkdir(parents=True, exist_ok=True)
    if args.command == "run":
        target_dir = resolve_path(args.target_dir, repo)
        document = benchmark(repo, output, target_dir, args.suite)
        (output / "raw-report.json").write_text(
            json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        (output / "report.md").write_text(render_run_report(document), encoding="utf-8")
        print(f"pVisor benchmark report: {output / 'report.md'}")
        return 0

    candidate = load_report(resolve_path(args.candidate, repo))
    baseline = load_report(resolve_path(args.baseline, repo)) if args.baseline else None
    comparison = compare_reports(candidate, baseline, args.regression_threshold)
    (output / "comparison.json").write_text(
        json.dumps(comparison, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (output / "report.md").write_text(render_comparison(comparison), encoding="utf-8")
    print(f"pVisor comparison report: {output / 'report.md'}")
    if args.fail_on_regression and comparison["regressions"]:
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
