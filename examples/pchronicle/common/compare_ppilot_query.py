#!/usr/bin/env python3
import json
import math
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

if len(sys.argv) not in (9, 10):
    raise SystemExit(
        "usage: compare_ppilot_query.py INPUT LANCE PYTHON_OUTPUT "
        "DIRECT_OUTPUT LANCE_OUTPUT ITERATIONS {group|selective} SQL [SESSION_ID]"
    )

(
    json_input,
    lance_input,
    python_output_arg,
    direct_output_arg,
    lance_output_arg,
    iterations_arg,
    query_kind,
    sql,
    *query_args,
) = sys.argv[1:]
iterations = int(iterations_arg)
if iterations <= 0:
    raise SystemExit("ITERATIONS must be greater than zero")
if query_kind == "group" and query_args:
    raise SystemExit("group baseline takes no SESSION_ID")
if query_kind == "selective" and len(query_args) != 1:
    raise SystemExit("selective baseline requires one SESSION_ID")
if query_kind not in ("group", "selective"):
    raise SystemExit(f"unsupported Python JSON baseline query: {query_kind}")

baseline_script = Path(__file__).with_name("python_json_baseline.py")
commands = {
    "python_json": [
        sys.executable,
        str(baseline_script),
        json_input,
        query_kind,
        *query_args,
    ],
    "pchronicle_json": [
        "ppilot",
        "query",
        "sql",
        json_input,
        "--query-metrics",
        "--sql",
        sql,
    ],
    "pchronicle_lance": ["ppilot", "query", "sql", lance_input, "--sql", sql],
}
samples: dict[str, list[float]] = {name: [] for name in commands}
rss_samples: dict[str, list[int]] = {name: [] for name in commands}
outputs: dict[str, bytes] = {}
engine_metrics: dict[str, dict] = {}
names = tuple(commands)


def peak_rss_bytes(raw_rss: int) -> int:
    # getrusage reports bytes on macOS and KiB on Linux/BSD.
    return raw_rss if sys.platform == "darwin" else raw_rss * 1024


def run_measured(command: list[str]) -> tuple[int, bytes, bytes, int, float]:
    """Run one isolated process and return output plus that child's peak RSS."""
    if not hasattr(os, "wait4"):
        raise SystemExit("this benchmark requires os.wait4 for per-process peak RSS measurement")
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        started = time.perf_counter()
        process = subprocess.Popen(command, stdout=stdout, stderr=stderr)
        _, status, usage = os.wait4(process.pid, 0)
        elapsed = time.perf_counter() - started
        process.returncode = os.waitstatus_to_exitcode(status)
        stdout.seek(0)
        stderr.seek(0)
        return (
            process.returncode,
            stdout.read(),
            stderr.read(),
            peak_rss_bytes(usage.ru_maxrss),
            elapsed,
        )


def parse_engine_metrics(stderr: bytes) -> dict:
    for line in reversed(stderr.splitlines()):
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and "source_bytes_read" in value:
            return value
    raise SystemExit(
        f"pChronicle JSON query did not emit metrics: {stderr.decode(errors='replace')}"
    )


for iteration in range(iterations):
    # Rotate all three paths so process warm-up and filesystem cache effects are
    # not systematically assigned to one implementation.
    offset = iteration % len(names)
    order = names[offset:] + names[:offset]
    for path_name in order:
        returncode, stdout, stderr, peak_rss, elapsed = run_measured(commands[path_name])
        samples[path_name].append(elapsed)
        rss_samples[path_name].append(peak_rss)
        if returncode != 0:
            sys.stderr.buffer.write(stderr)
            raise SystemExit(returncode)
        if path_name == "pchronicle_json":
            engine_metrics[path_name] = parse_engine_metrics(stderr)
        previous = outputs.setdefault(path_name, stdout)
        if stdout != previous:
            raise SystemExit(f"{path_name} output changed between timing iterations")


def semantic_rows(output: bytes) -> list[dict]:
    return [json.loads(line) for line in output.splitlines() if line.strip()]


def percentile_ms(values: list[float], percentile: float) -> float:
    ordered = sorted(values)
    index = max(0, math.ceil(percentile * len(ordered)) - 1)
    return ordered[index] * 1000


python_rows = semantic_rows(outputs["python_json"])
equal = all(semantic_rows(output) == python_rows for output in outputs.values())
Path(python_output_arg).write_bytes(outputs["python_json"])
Path(direct_output_arg).write_bytes(outputs["pchronicle_json"])
Path(lance_output_arg).write_bytes(outputs["pchronicle_lance"])

with Path(json_input).open(encoding="utf-8") as source:
    input_rows = sum(len(json.loads(line).get("steps", [])) for line in source if line.strip())

metrics = {
    name: {
        "median_ms": percentile_ms(values, 0.50),
        "p95_ms": percentile_ms(values, 0.95),
        "rows_per_second": input_rows / (percentile_ms(values, 0.50) / 1000),
        "peak_rss_mib": max(rss_samples[name]) / (1024 * 1024),
    }
    for name, values in samples.items()
}
baseline_ms = metrics["python_json"]["median_ms"]
for name in ("pchronicle_json", "pchronicle_lance"):
    metrics[name]["speedup_vs_python"] = baseline_ms / metrics[name]["median_ms"]

print(
    json.dumps(
        {
            "equal": equal,
            "iterations": iterations,
            "input_rows": input_rows,
            **{
                name: {key: round(value, 3) for key, value in values.items()}
                for name, values in metrics.items()
            },
            "engine_metrics": engine_metrics,
        },
        separators=(",", ":"),
    )
)
