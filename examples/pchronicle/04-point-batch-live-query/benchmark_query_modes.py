#!/usr/bin/env python3
import json
import math
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path


def run(command: list[str]) -> str:
    completed = subprocess.run(command, text=True, capture_output=True)
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr)
        raise SystemExit(completed.returncode)
    return completed.stdout


def timed_commands(commands: list[list[str]], iterations: int) -> tuple[float, list[str]]:
    outputs: list[str] = []
    started = time.perf_counter()
    for _ in range(iterations):
        outputs = [run(command) for command in commands]
    return (time.perf_counter() - started) / iterations, outputs


def jsonl_rows(documents: list[str]) -> list[dict]:
    return [json.loads(line) for document in documents for line in document.splitlines() if line]


def percentile(values: list[float], percentile_value: float) -> float:
    ordered = sorted(values)
    index = max(0, math.ceil(percentile_value * len(ordered)) - 1)
    return ordered[index]


def event(index: int, emitted_unix_ns: int) -> dict:
    return {
        "seq": 0,
        "source": "query-mode-benchmark",
        "kind": "benchmark.event",
        "timestamp": None,
        "session_id": None,
        "agent_id": None,
        "parent_uuid": None,
        "trace_id": None,
        "call_id": None,
        "subagent_id": None,
        "parent_agent_id": None,
        "branch": None,
        "parent_call_id": None,
        "payload": {"benchmark_index": index, "emitted_unix_ns": emitted_unix_ns},
    }


def live_visibility_benchmark(
    work_dir: Path,
    batches: int,
    batch_size: int,
    poll_ms: int,
) -> tuple[list[float], float]:
    live_store = work_dir / "live"
    agent_id = "query-mode-benchmark"
    session_id = "live-run"
    command = [
        "ppilot",
        "query",
        "follow",
        str(live_store),
        "--agent-id",
        agent_id,
        "--session-id",
        session_id,
        "--poll-interval-ms",
        str(poll_ms),
        "--limit",
        "64",
    ]
    follower = subprocess.Popen(
        command,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=1,
    )
    assert follower.stdout is not None
    assert follower.stderr is not None
    received: list[tuple[int, int, int]] = []
    reader_errors: list[str] = []
    condition = threading.Condition()

    def read_follow_output() -> None:
        try:
            for line in follower.stdout:
                arrived_unix_ns = time.time_ns()
                record = json.loads(line)
                payload = record["payload"]
                with condition:
                    received.append(
                        (
                            int(payload["benchmark_index"]),
                            int(payload["emitted_unix_ns"]),
                            arrived_unix_ns,
                        )
                    )
                    condition.notify_all()
        except Exception as error:  # surfaced in the main thread below
            with condition:
                reader_errors.append(str(error))
                condition.notify_all()

    reader = threading.Thread(target=read_follow_output, daemon=True)
    reader.start()
    # Deliberately begin following before the first Lance dataset exists.
    time.sleep(max(0.02, poll_ms / 1000 * 2))

    expected = batches * batch_size
    next_index = 0
    for batch_index in range(batches):
        records = []
        for _ in range(batch_size):
            emitted = time.time_ns()
            records.append(event(next_index, emitted))
            next_index += 1
        batch_path = work_dir / f"live-batch-{batch_index:03}.jsonl"
        batch_path.write_text(
            "".join(json.dumps(record, separators=(",", ":")) + "\n" for record in records),
            encoding="utf-8",
        )
        run(
            [
                "persisting",
                "history",
                "add",
                str(live_store),
                "--agent-id",
                agent_id,
                "--session-id",
                session_id,
                "--format",
                "jsonl",
                "--input",
                str(batch_path),
            ]
        )

    deadline = time.monotonic() + 30
    with condition:
        while len(received) < expected and not reader_errors:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            condition.wait(remaining)

    follower.send_signal(signal.SIGINT)
    try:
        return_code = follower.wait(timeout=5)
    except subprocess.TimeoutExpired:
        follower.kill()
        raise SystemExit("follow process did not exit after SIGINT")
    reader.join(timeout=2)
    stderr = follower.stderr.read()
    if return_code != 0:
        sys.stderr.write(stderr)
        raise SystemExit(f"follow process exited with {return_code}")
    if reader_errors:
        raise SystemExit(f"follow JSONL reader failed: {reader_errors[0]}")
    if len(received) != expected:
        raise SystemExit(f"follow returned {len(received)} of {expected} events; stderr={stderr!r}")

    indexes = [item[0] for item in received]
    if indexes != list(range(expected)):
        raise SystemExit("follow output was duplicated, missing, or out of order")
    latencies_ms = [(arrived - emitted) / 1_000_000 for _, emitted, arrived in received]
    elapsed_seconds = (received[-1][2] - received[0][1]) / 1_000_000_000
    return latencies_ms, expected / elapsed_seconds


if len(sys.argv) != 9:
    raise SystemExit(
        "usage: benchmark_query_modes.py INPUT STORE WORK_DIR "
        "ITERATIONS BATCH_IDS LIVE_BATCHES LIVE_BATCH_SIZE FOLLOW_POLL_MS"
    )

input_arg, store_arg, work_arg = sys.argv[1:4]
iterations, batch_size, live_batches, live_batch_size, poll_ms = map(int, sys.argv[4:])
if min(iterations, batch_size, live_batches, live_batch_size, poll_ms) <= 0:
    raise SystemExit("all numeric benchmark arguments must be greater than zero")

input_path = Path(input_arg)
work_dir = Path(work_arg)
trajectories = [json.loads(line) for line in input_path.read_text().splitlines() if line.strip()]
if len(trajectories) < batch_size:
    raise SystemExit(f"benchmark needs at least {batch_size} trajectories")

print(run(["ppilot", "chronicle", "import", input_arg, store_arg]).strip())
_, target = max(enumerate(trajectories), key=lambda item: (len(item[1]["steps"]), item[0]))
target_session = target["session_id"]
target_step = target["steps"][len(target["steps"]) // 2]["step_id"]
batch_sessions = [trajectory["session_id"] for trajectory in trajectories[:batch_size]]
point_command = [
    "ppilot",
    "query",
    "point",
    store_arg,
    "--session-id",
    target_session,
    "--step-id",
    str(target_step),
]
trajectory_command = [
    "ppilot",
    "query",
    "point",
    store_arg,
    "--session-id",
    target_session,
]
point_batch_commands = [
    [
        "ppilot",
        "query",
        "point",
        store_arg,
        "--session-id",
        session_id,
        "--step-id",
        "1",
    ]
    for session_id in batch_sessions
]
batch_command = [
    "ppilot",
    "query",
    "batch",
    store_arg,
    "--session-id",
    ",".join(batch_sessions),
    "--step-id",
    "1",
]

point_seconds, point_output = timed_commands([point_command], iterations)
trajectory_seconds, trajectory_output = timed_commands([trajectory_command], iterations)
individual_seconds, individual_outputs = timed_commands(point_batch_commands, iterations)
batch_seconds, batch_output = timed_commands([batch_command], iterations)
individual_rows = sorted(jsonl_rows(individual_outputs), key=lambda row: row["session_id"])
batch_rows = jsonl_rows(batch_output)
if individual_rows != batch_rows or len(batch_rows) != batch_size:
    raise SystemExit("batch query result differs from the equivalent point queries")

live_latencies_ms, live_events_per_second = live_visibility_benchmark(
    work_dir, live_batches, live_batch_size, poll_ms
)
point_ms = point_seconds * 1000
trajectory_ms = trajectory_seconds * 1000
individual_ms = individual_seconds * 1000
batch_ms = batch_seconds * 1000
cli_batching_gain = individual_seconds / batch_seconds
live_p50 = percentile(live_latencies_ms, 0.50)
live_p95 = percentile(live_latencies_ms, 0.95)
live_max = max(live_latencies_ms)

print(
    f"dataset: {len(trajectories)} trajectories, {sum(len(t['steps']) for t in trajectories)} steps"
)
print(f"point step: {point_ms:.3f} ms cold CLI mean; rows={len(jsonl_rows(point_output))}")
print(
    f"point trajectory: {trajectory_ms:.3f} ms cold CLI mean; "
    f"storylines={len(jsonl_rows(trajectory_output))}"
)
print(
    f"batch {batch_size}: {batch_ms:.3f} ms in one IN query vs "
    f"{individual_ms:.3f} ms for {batch_size} point CLI calls "
    f"({cli_batching_gain:.2f}x CLI batching gain)"
)
print(
    f"live follow: events={len(live_latencies_ms)} poll_ms={poll_ms} "
    f"p50={live_p50:.3f} ms p95={live_p95:.3f} ms max={live_max:.3f} ms "
    f"throughput={live_events_per_second:.1f} events/s"
)
print("Conclusion:")
print("  Point and batch queries returned equivalent rows from one committed Storyline snapshot.")
print(
    "  One batch query amortized process startup, store open, planning, and execution across all ids."
)
print(
    "  Live follow started before the dataset existed and emitted every committed event exactly once."
)
print(
    "  These are local release-CLI measurements for different result shapes; "
    "they are not universal backend latency claims."
)
print(
    "RESULT benchmark=query_modes "
    f"trajectories={len(trajectories)} steps={sum(len(t['steps']) for t in trajectories)} "
    f"iterations={iterations} batch_ids={batch_size} "
    f"point_step_ms={point_ms:.3f} trajectory_ms={trajectory_ms:.3f} "
    f"individual_batch_ms={individual_ms:.3f} batch_ms={batch_ms:.3f} "
    f"cli_batching_gain={cli_batching_gain:.3f} live_events={len(live_latencies_ms)} "
    f"follow_poll_ms={poll_ms} live_p50_ms={live_p50:.3f} "
    f"live_p95_ms={live_p95:.3f} live_max_ms={live_max:.3f} "
    f"live_events_per_second={live_events_per_second:.1f} equal=true"
)
