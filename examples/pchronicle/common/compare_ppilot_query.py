#!/usr/bin/env python3
import json
import math
import subprocess
import sys
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
    "pchronicle_json": ["ppilot", "query", json_input, "--sql", sql],
    "pchronicle_lance": ["ppilot", "query", lance_input, "--sql", sql],
}
samples: dict[str, list[float]] = {name: [] for name in commands}
outputs: dict[str, bytes] = {}
names = tuple(commands)

for iteration in range(iterations):
    # Rotate all three paths so process warm-up and filesystem cache effects are
    # not systematically assigned to one implementation.
    offset = iteration % len(names)
    order = names[offset:] + names[:offset]
    for path_name in order:
        started = time.perf_counter()
        completed = subprocess.run(commands[path_name], stdout=subprocess.PIPE)
        samples[path_name].append(time.perf_counter() - started)
        if completed.returncode != 0:
            raise SystemExit(completed.returncode)
        previous = outputs.setdefault(path_name, completed.stdout)
        if completed.stdout != previous:
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

metrics = {
    name: {
        "median_ms": percentile_ms(values, 0.50),
        "p95_ms": percentile_ms(values, 0.95),
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
            **{
                name: {key: round(value, 3) for key, value in values.items()}
                for name, values in metrics.items()
            },
        },
        separators=(",", ":"),
    )
)
