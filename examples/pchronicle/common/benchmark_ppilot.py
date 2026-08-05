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


def timed(command: list[str], iterations: int) -> tuple[float, str]:
    output = ""
    started = time.perf_counter()
    for _ in range(iterations):
        output = run_ppilot(command)
    return (time.perf_counter() - started) / iterations, output


def total_bytes(path: Path) -> int:
    if path.is_file():
        return path.stat().st_size
    return sum(item.stat().st_size for item in path.rglob("*") if item.is_file())


def speed_conclusion(lance_seconds: float, atif_seconds: float) -> str:
    if lance_seconds <= atif_seconds:
        return f"Lance is {atif_seconds / lance_seconds:.2f}x faster"
    return f"ATIF is {lance_seconds / atif_seconds:.2f}x faster"


if len(sys.argv) != 4:
    raise SystemExit("usage: benchmark_ppilot.py INPUT STORE ITERATIONS")

input_arg, store_arg, iterations_arg = sys.argv[1:]
ppilot = "ppilot"
input_path = Path(input_arg)
store_path = Path(store_arg)
iterations = int(iterations_arg)
if iterations <= 0:
    raise SystemExit("ITERATIONS must be greater than zero")

trajectories = [
    json.loads(line) for line in input_path.read_text().splitlines() if line.strip()
]
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


def query_command(input_value: str, sql: str) -> list[str]:
    return [ppilot, "query", input_value, "--sql", sql]


lance_selective, lance_selective_output = timed(
    query_command(store_arg, selective_sql), iterations
)
atif_selective, atif_selective_output = timed(
    query_command(input_arg, selective_sql), iterations
)
lance_group, lance_group_output = timed(
    query_command(store_arg, group_sql), iterations
)
atif_group, atif_group_output = timed(
    query_command(input_arg, group_sql), iterations
)
if lance_selective_output != atif_selective_output:
    raise SystemExit("selective query result differs between Lance and ATIF")
if lance_group_output != atif_group_output:
    raise SystemExit("GROUP BY result differs between Lance and ATIF")

replacement = dict(target)
replacement["notes"] = "pPilot CLI incremental replacement benchmark"
replacement_path = store_path.parent / "replacement.ndjson"
replacement_path.write_text(json.dumps(replacement, separators=(",", ":")) + "\n")
replace_started = time.perf_counter()
run_ppilot([ppilot, "chronicle", "import", str(replacement_path), store_arg])
replace_seconds = time.perf_counter() - replace_started

atif_bytes = total_bytes(input_path)
lance_bytes = total_bytes(store_path)
lance_ratio = lance_bytes / atif_bytes
selective_ratio = atif_selective / lance_selective
group_ratio = atif_group / lance_group

print(f"dataset: {len(trajectories)} trajectories, {sum(len(t['steps']) for t in trajectories)} steps")
print(f"storage: ATIF={atif_bytes} bytes, Lance store={lance_bytes} bytes ({lance_ratio:.3f}x)")
print(f"pPilot CLI import: {import_seconds * 1000:.3f} ms")
print(f"pPilot CLI single-story replace: {replace_seconds * 1000:.3f} ms")
print("cold pPilot query (process start + open + plan + execute):")
print(
    f"  selective: Lance={lance_selective * 1000:.3f} ms, "
    f"ATIF={atif_selective * 1000:.3f} ms"
)
print(
    f"  GROUP BY:  Lance={lance_group * 1000:.3f} ms, "
    f"ATIF={atif_group * 1000:.3f} ms"
)
print("Conclusion:")
print(
    f"  Storage: Lance uses {lance_ratio * 100:.2f}% of ATIF space, "
    f"saving {(1 - lance_ratio) * 100:.2f}%."
)
print(f"  Selective cold CLI query: {speed_conclusion(lance_selective, atif_selective)}.")
print(f"  GROUP BY cold CLI query: {speed_conclusion(lance_group, atif_group)}.")
print(
    f"  Import took {import_seconds * 1000:.3f} ms; "
    f"one Storyline replacement took {replace_seconds * 1000:.3f} ms."
)
print("  Query results were identical between the Lance and ATIF pPilot backends.")
print(
    "RESULT benchmark=ppilot_cli "
    f"iterations={iterations} lance_over_atif_size={lance_ratio:.4f} "
    f"import_ms={import_seconds * 1000:.3f} replace_ms={replace_seconds * 1000:.3f} "
    f"selective_lance_ms={lance_selective * 1000:.3f} "
    f"selective_atif_ms={atif_selective * 1000:.3f} "
    f"selective_atif_over_lance={selective_ratio:.3f} "
    f"group_lance_ms={lance_group * 1000:.3f} "
    f"group_atif_ms={atif_group * 1000:.3f} "
    f"group_atif_over_lance={group_ratio:.3f} equal=true"
)
