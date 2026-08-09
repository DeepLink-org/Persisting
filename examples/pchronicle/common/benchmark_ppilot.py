#!/usr/bin/env python3
import json
import subprocess
import sys
import time
from pathlib import Path


def run_ppilot(command: list[str]) -> str:
    completed = subprocess.run(command, text=True, capture_output=True)
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr)
        raise SystemExit(completed.returncode)
    return completed.stdout


def total_bytes(path: Path) -> int:
    if path.is_file():
        return path.stat().st_size
    return sum(item.stat().st_size for item in path.rglob("*") if item.is_file())


def compare_query_paths(
    input_arg: str,
    store_arg: str,
    output_dir: Path,
    iterations: int,
    query_kind: str,
    sql: str,
    session_id: str | None = None,
) -> dict:
    script = Path(__file__).with_name("compare_ppilot_query.py")
    command = [
        sys.executable,
        str(script),
        input_arg,
        store_arg,
        str(output_dir / f"{query_kind}-python.jsonl"),
        str(output_dir / f"{query_kind}-pchronicle-json.jsonl"),
        str(output_dir / f"{query_kind}-pchronicle-lance.jsonl"),
        str(iterations),
        query_kind,
        sql,
    ]
    if session_id is not None:
        command.append(session_id)
    return json.loads(run_ppilot(command))


def print_query(name: str, metrics: dict) -> None:
    python = metrics["python_json"]
    direct = metrics["pchronicle_json"]
    lance = metrics["pchronicle_lance"]
    print(f"  {name}:")
    print(
        f"    Python JSON baseline: {python['median_ms']:.3f} ms median, "
        f"p95={python['p95_ms']:.3f} ms"
    )
    print(
        f"    pChronicle JSON:      {direct['median_ms']:.3f} ms median, "
        f"p95={direct['p95_ms']:.3f} ms, "
        f"{direct['speedup_vs_python']:.3f}x vs baseline"
    )
    print(
        f"    pChronicle Lance:     {lance['median_ms']:.3f} ms median, "
        f"p95={lance['p95_ms']:.3f} ms, "
        f"{lance['speedup_vs_python']:.3f}x vs baseline"
    )


if len(sys.argv) != 4:
    raise SystemExit("usage: benchmark_ppilot.py INPUT STORE ITERATIONS")

input_arg, store_arg, iterations_arg = sys.argv[1:]
ppilot = "ppilot"
input_path = Path(input_arg)
store_path = Path(store_arg)
iterations = int(iterations_arg)
if iterations <= 0:
    raise SystemExit("ITERATIONS must be greater than zero")

trajectories = [json.loads(line) for line in input_path.read_text().splitlines() if line.strip()]
if not trajectories:
    raise SystemExit("benchmark ATIF input is empty")
target = max(enumerate(trajectories), key=lambda item: (len(item[1]["steps"]), item[0]))[1]
target_session = target["session_id"]

import_started = time.perf_counter()
import_output = run_ppilot([ppilot, "chronicle", "import", input_arg, store_arg]).strip()
import_seconds = time.perf_counter() - import_started
print(import_output)

selective_sql = (
    "SELECT step_id, source FROM steps "
    f"WHERE session_id = '{target_session}' AND step_id BETWEEN 5 AND 15 "
    "ORDER BY step_id"
)
group_sql = "SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source"


selective = compare_query_paths(
    input_arg,
    store_arg,
    store_path.parent,
    iterations,
    "selective",
    selective_sql,
    target_session,
)
group = compare_query_paths(input_arg, store_arg, store_path.parent, iterations, "group", group_sql)
if not selective["equal"] or not group["equal"]:
    raise SystemExit("query result differs across Python, pChronicle JSON, and Lance paths")

atif_bytes = total_bytes(input_path)
lance_bytes = total_bytes(store_path)
lance_ratio = lance_bytes / atif_bytes

print(
    f"dataset: {len(trajectories)} trajectories, {sum(len(t['steps']) for t in trajectories)} steps"
)
print(
    f"storage: raw JSON baseline={atif_bytes} bytes, "
    f"pChronicle Lance={lance_bytes} bytes ({lance_ratio:.3f}x)"
)
print(f"pChronicle Lance import: {import_seconds * 1000:.3f} ms")
print(
    "cold process query (process start + input parse/open + query; "
    "speedup = Python median / path median):"
)
print_query("selective", selective)
print_query("GROUP BY", group)
print("Conclusion:")
print(
    f"  Storage: pChronicle Lance uses {lance_ratio * 100:.2f}% of raw JSON space, "
    f"saving {(1 - lance_ratio) * 100:.2f}%."
)
print(
    "  Python json.loads plus a native loop is the raw-file baseline. "
    "Both pChronicle paths are measured against it, not against each other."
)
print(f"  Building the reusable pChronicle Lance store took {import_seconds * 1000:.3f} ms.")
print("  Query results were identical across all three measured paths.")
print(
    "RESULT benchmark=pchronicle_query_paths query_baseline=python_json "
    f"storage_baseline=raw_json iterations={iterations} "
    f"lance_over_json_size={lance_ratio:.4f} "
    f"import_ms={import_seconds * 1000:.3f} "
    f"selective_python_ms={selective['python_json']['median_ms']:.3f} "
    f"selective_python_p95_ms={selective['python_json']['p95_ms']:.3f} "
    f"selective_pchronicle_json_ms={selective['pchronicle_json']['median_ms']:.3f} "
    f"selective_pchronicle_json_p95_ms={selective['pchronicle_json']['p95_ms']:.3f} "
    f"selective_pchronicle_lance_ms={selective['pchronicle_lance']['median_ms']:.3f} "
    f"selective_pchronicle_lance_p95_ms={selective['pchronicle_lance']['p95_ms']:.3f} "
    f"selective_json_vs_python={selective['pchronicle_json']['speedup_vs_python']:.3f} "
    f"selective_lance_vs_python={selective['pchronicle_lance']['speedup_vs_python']:.3f} "
    f"group_python_ms={group['python_json']['median_ms']:.3f} "
    f"group_python_p95_ms={group['python_json']['p95_ms']:.3f} "
    f"group_pchronicle_json_ms={group['pchronicle_json']['median_ms']:.3f} "
    f"group_pchronicle_json_p95_ms={group['pchronicle_json']['p95_ms']:.3f} "
    f"group_pchronicle_lance_ms={group['pchronicle_lance']['median_ms']:.3f} "
    f"group_pchronicle_lance_p95_ms={group['pchronicle_lance']['p95_ms']:.3f} "
    f"group_json_vs_python={group['pchronicle_json']['speedup_vs_python']:.3f} "
    f"group_lance_vs_python={group['pchronicle_lance']['speedup_vs_python']:.3f} equal=true"
)
