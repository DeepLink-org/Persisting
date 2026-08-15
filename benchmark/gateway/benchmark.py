#!/usr/bin/env python3
"""Black-box throughput benchmark for Gateway forwarding plus durable capture."""

from __future__ import annotations

import argparse
import concurrent.futures
import http.client
import json
import os
import platform
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import uuid
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import TextIO

SCENARIO_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCENARIO_DIR.parents[1]
sys.path.insert(0, str(REPO_ROOT / "tests" / "regression"))

from gateway_harness import (  # noqa: E402
    require_subcommand,
    resolve_binary,
    stop_process,
    wait_http,
    wait_logged_url,
    without_proxy_environment,
)


@dataclass
class LoadResult:
    elapsed_seconds: float
    requests: int
    errors: int
    requests_per_second: float
    latency_ms_mean: float
    latency_ms_p50: float
    latency_ms_p95: float
    latency_ms_p99: float
    latency_ms_max: float
    estimated_mean_in_flight: float
    error_samples: list[str]


def percentile(values: list[float], quantile: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, int(len(ordered) * quantile + 0.999999) - 1))
    return ordered[index]


def one_worker(
    base_url: str,
    start: threading.Event,
    deadline: list[float],
    request_budget: list[int] | None,
    request_budget_lock: threading.Lock,
    worker: int,
    concurrency: int,
    sessions: int,
    session_prefix: str,
    payload: str,
) -> tuple[list[float], int, list[str]]:
    parsed = urllib.parse.urlparse(base_url)
    if parsed.scheme != "http" or parsed.hostname is None or parsed.port is None:
        raise ValueError(f"benchmark target must be an explicit HTTP URL: {base_url}")
    path = f"{parsed.path.rstrip('/')}/v1/chat/completions"
    body = json.dumps(
        {
            "model": "gateway-benchmark",
            "messages": [{"role": "user", "content": payload}],
            "stream": False,
        },
        separators=(",", ":"),
    ).encode()
    headers = {
        "authorization": "Bearer benchmark-local",
        "content-type": "application/json",
    }
    latencies: list[float] = []
    error_count = 0
    errors: list[str] = []
    connection: http.client.HTTPConnection | None = None
    request_index = 0
    start.wait()
    while time.monotonic() < deadline[0]:
        if request_budget is not None:
            with request_budget_lock:
                if request_budget[0] <= 0:
                    break
                request_budget[0] -= 1
        request_started = time.perf_counter()
        try:
            if connection is None:
                connection = http.client.HTTPConnection(parsed.hostname, parsed.port, timeout=15)
            # Keep storage topology constant across a concurrency sweep. Advancing
            # by `concurrency` makes all workers collectively round-robin over the
            # configured session set without changing its cardinality.
            session = (worker + request_index * concurrency) % sessions
            headers["x-persisting-benchmark-session"] = f"{session_prefix}-{session}"
            request_index += 1
            connection.request("POST", path, body=body, headers=headers)
            response = connection.getresponse()
            response_body = response.read()
            if response.status != 200:
                raise RuntimeError(f"HTTP {response.status}: {response_body[:160]!r}")
            document = json.loads(response_body)
            content = document["choices"][0]["message"]["content"]
            if content != payload:
                raise RuntimeError("echo response content mismatch")
            latencies.append((time.perf_counter() - request_started) * 1000.0)
        except Exception as error:  # benchmark must report transport and protocol failures
            error_count += 1
            if len(errors) < 10:
                errors.append(f"worker {worker}: {error}")
            if connection is not None:
                connection.close()
                connection = None
    if connection is not None:
        connection.close()
    return latencies, error_count, errors


def run_load(
    base_url: str,
    *,
    duration: float,
    concurrency: int,
    sessions: int,
    max_requests: int | None,
    session_prefix: str,
    payload: str,
) -> LoadResult:
    start = threading.Event()
    deadline = [0.0]
    request_budget = [max_requests] if max_requests is not None else None
    request_budget_lock = threading.Lock()
    started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as executor:
        futures = [
            executor.submit(
                one_worker,
                base_url,
                start,
                deadline,
                request_budget,
                request_budget_lock,
                worker,
                concurrency,
                sessions,
                session_prefix,
                payload,
            )
            for worker in range(concurrency)
        ]
        deadline[0] = time.monotonic() + duration
        start.set()
        worker_results = [future.result() for future in futures]
    elapsed = time.perf_counter() - started
    latencies = [latency for worker, _, _ in worker_results for latency in worker]
    error_count = sum(count for _, count, _ in worker_results)
    errors = [error for _, _, worker_errors in worker_results for error in worker_errors][:10]
    return LoadResult(
        elapsed_seconds=elapsed,
        requests=len(latencies),
        errors=error_count,
        requests_per_second=len(latencies) / elapsed if elapsed else 0.0,
        latency_ms_mean=statistics.fmean(latencies) if latencies else 0.0,
        latency_ms_p50=percentile(latencies, 0.50),
        latency_ms_p95=percentile(latencies, 0.95),
        latency_ms_p99=percentile(latencies, 0.99),
        latency_ms_max=max(latencies, default=0.0),
        # Little's law: throughput * residence time estimates how many requests
        # were resident in the measured path on average. In this closed-loop
        # driver, a value near `concurrency` means the client kept the path full.
        estimated_mean_in_flight=(
            (len(latencies) / elapsed) * statistics.fmean(latencies) / 1000.0
            if elapsed and latencies
            else 0.0
        ),
        error_samples=errors,
    )


def write_configs(work_dir: Path, echo_url: str) -> tuple[Path, Path]:
    warehouse = work_dir / "warehouse.toml"
    gateway = work_dir / "gateway.toml"
    warehouse.write_text(
        f'''default_dataset = "captures"

[[datasets]]
name = "captures"
uri = "{work_dir / "dataset"}"
''',
        encoding="utf-8",
    )
    gateway.write_text(
        f'''listen = "127.0.0.1:0"
admin_listen = "127.0.0.1:0"
agent_id = "gateway-benchmark"
session_header = "x-persisting-benchmark-session"
capture_level = "full"

[[models]]
name = "gateway-benchmark"
provider = "openai"
upstream = "{echo_url}/v1"
''',
        encoding="utf-8",
    )
    return warehouse, gateway


def captured_manifest_stats(dataset: Path, agent_id: str, prefix: str) -> dict[str, int]:
    """Count published capture rows without opening every Lance version.

    The epoch-fenced manifest is the canonical visibility pointer for an event
    log. Reading one manifest at a time keeps file-descriptor use constant even
    when a benchmark produces many sessions and Lance versions.
    """
    agent_dir = dataset / agent_id
    if not agent_dir.is_dir():
        raise RuntimeError(f"capture agent directory is missing: {agent_dir}")

    session_prefix = f"{prefix}-"
    sessions = sorted(
        path
        for path in agent_dir.iterdir()
        if path.is_dir() and path.name.startswith(session_prefix)
    )
    if not sessions:
        raise RuntimeError(f"no capture sessions found for prefix {prefix!r}")

    published_events = 0
    visible_segments = 0
    max_segment_level = 0
    min_revision: int | None = None
    max_revision: int | None = None
    for session in sessions:
        manifest_path = session / "events.lance" / "_manifest.json"
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        except FileNotFoundError as error:
            raise RuntimeError(f"capture manifest is missing: {manifest_path}") from error
        except (OSError, json.JSONDecodeError) as error:
            raise RuntimeError(f"cannot read capture manifest {manifest_path}: {error}") from error

        if manifest.get("schema_version") != 1:
            raise RuntimeError(
                f"unsupported capture manifest schema in {manifest_path}: "
                f"{manifest.get('schema_version')!r}"
            )
        revision = manifest.get("revision")
        segments = manifest.get("segments")
        if (
            not isinstance(revision, int)
            or isinstance(revision, bool)
            or revision < 0
            or not isinstance(segments, list)
        ):
            raise RuntimeError(f"invalid capture manifest metadata: {manifest_path}")

        for segment in segments:
            rows = segment.get("rows") if isinstance(segment, dict) else None
            level = segment.get("level", 0) if isinstance(segment, dict) else None
            if not isinstance(rows, int) or isinstance(rows, bool) or rows < 0:
                raise RuntimeError(f"invalid segment row count in {manifest_path}: {rows!r}")
            if not isinstance(level, int) or isinstance(level, bool) or level < 0:
                raise RuntimeError(f"invalid segment level in {manifest_path}: {level!r}")
            published_events += rows
            max_segment_level = max(max_segment_level, level)
        visible_segments += len(segments)
        min_revision = revision if min_revision is None else min(min_revision, revision)
        max_revision = revision if max_revision is None else max(max_revision, revision)

    return {
        "sessions": len(sessions),
        "visible_segments": visible_segments,
        "max_segment_level": max_segment_level,
        "published_events": published_events,
        "min_manifest_revision": min_revision or 0,
        "max_manifest_revision": max_revision or 0,
    }


def captured_event_counts(
    pchronicle: Path, dataset: Path, prefix: str, output: Path
) -> dict[str, int]:
    """Exercise the real query path after storage-fragment maintenance."""
    query = (
        "SELECT kind, COUNT(*) AS count FROM dataset.events "
        f"WHERE session_id LIKE '{prefix}%' GROUP BY kind ORDER BY kind"
    )
    subprocess.run(
        [
            str(pchronicle),
            "query",
            str(dataset),
            query,
            "--format",
            "jsonl",
            "--output",
            str(output),
        ],
        cwd=REPO_ROOT,
        check=True,
    )
    counts: dict[str, int] = {}
    for line in output.read_text(encoding="utf-8").splitlines():
        row = json.loads(line)
        counts[str(row["kind"])] = int(row["count"])
    return counts


def benchmark(args: argparse.Namespace, work_dir: Path, pchronicle: Path) -> dict[str, object]:
    logs = work_dir / "logs"
    dataset = work_dir / "dataset"
    state = work_dir / "gateway-state"
    logs.mkdir()
    dataset.mkdir()
    state.mkdir()
    payload = "x" * args.payload_bytes
    run_token = uuid.uuid4().hex
    measured_prefix = f"gateway-bench-{run_token}"
    processes: list[tuple[str, subprocess.Popen[bytes]]] = []
    handles: list[TextIO] = []
    serve_process: subprocess.Popen[bytes] | None = None
    try:
        echo_log = (logs / "echo.log").open("w", encoding="utf-8")
        handles.append(echo_log)
        echo_process = subprocess.Popen(
            [str(pchronicle), "echo", "--listen", "127.0.0.1:0", "--encoding", "plain"],
            cwd=REPO_ROOT,
            stdout=echo_log,
            stderr=subprocess.STDOUT,
        )
        processes.append(("pChronicle Echo", echo_process))
        echo_url = wait_logged_url(
            logs / "echo.log", "pChronicle Echo: ", echo_process, "pChronicle Echo"
        )
        wait_http(f"{echo_url}/health", echo_process, "pChronicle Echo")

        baseline: LoadResult | None = None
        with without_proxy_environment():
            if not args.skip_baseline:
                if args.warmup > 0:
                    warmup = run_load(
                        echo_url,
                        duration=args.warmup,
                        concurrency=args.concurrency,
                        sessions=args.sessions,
                        max_requests=min(args.requests, 64),
                        session_prefix=f"baseline-warmup-{run_token}",
                        payload=payload,
                    )
                    if warmup.errors:
                        raise RuntimeError(f"Echo baseline warmup failed: {warmup.error_samples}")
                baseline = run_load(
                    echo_url,
                    duration=args.duration,
                    concurrency=args.concurrency,
                    sessions=args.sessions,
                    max_requests=args.requests,
                    session_prefix=f"baseline-{run_token}",
                    payload=payload,
                )
                if baseline.errors:
                    raise RuntimeError(f"Echo baseline failed: {baseline.error_samples}")

        warehouse, gateway = write_configs(work_dir, echo_url)
        serve_log = (logs / "serve.log").open("w", encoding="utf-8")
        handles.append(serve_log)
        serve_process = subprocess.Popen(
            [
                str(pchronicle),
                "serve",
                "--config",
                str(warehouse),
                "--listen",
                "127.0.0.1:0",
                "--gateway",
                str(gateway),
                "--gateway-dataset",
                "captures",
                "--gateway-state",
                str(state),
            ],
            cwd=REPO_ROOT,
            stdout=serve_log,
            stderr=subprocess.STDOUT,
        )
        processes.append(("pChronicle serve", serve_process))
        gateway_url = wait_logged_url(
            logs / "serve.log", "pChronicle Gateway: ", serve_process, "pChronicle Gateway"
        )
        admin_url = wait_logged_url(
            logs / "serve.log",
            "pChronicle Gateway admin: ",
            serve_process,
            "pChronicle Gateway admin",
        )
        wait_http(f"{admin_url}/admin/status", serve_process, "pChronicle Gateway admin")

        with without_proxy_environment():
            if args.warmup > 0:
                warmup = run_load(
                    gateway_url,
                    duration=args.warmup,
                    concurrency=args.concurrency,
                    sessions=args.sessions,
                    max_requests=min(args.requests, 64),
                    session_prefix=f"gateway-warmup-{run_token}",
                    payload=payload,
                )
                if warmup.errors:
                    raise RuntimeError(f"Gateway warmup failed: {warmup.error_samples}")
            gateway_result = run_load(
                gateway_url,
                duration=args.duration,
                concurrency=args.concurrency,
                sessions=args.sessions,
                max_requests=args.requests,
                session_prefix=measured_prefix,
                payload=payload,
            )
        if gateway_result.errors:
            raise RuntimeError(f"Gateway load failed: {gateway_result.error_samples}")
        if gateway_result.requests_per_second < args.min_rps:
            raise RuntimeError(
                f"Gateway throughput {gateway_result.requests_per_second:.2f} req/s is below "
                f"--min-rps {args.min_rps:.2f}"
            )

        drain_started = time.perf_counter()
        stop_process(serve_process, label="pChronicle serve", require_success=True)
        shutdown_drain_seconds = time.perf_counter() - drain_started
        serve_process = None
        capture = captured_manifest_stats(dataset, "gateway-benchmark", measured_prefix)
        expected_requests = gateway_result.requests
        expected_events = expected_requests * 2
        if capture["published_events"] != expected_events:
            raise RuntimeError(
                "durable capture count mismatch: "
                f"requests={expected_requests}, expected_events={expected_events}, "
                f"capture={json.dumps(capture, sort_keys=True)}"
            )
        event_counts = captured_event_counts(
            pchronicle, dataset, measured_prefix, logs / "capture-counts.jsonl"
        )
        if (
            event_counts.get("llm.request", 0) != expected_requests
            or event_counts.get("llm.response", 0) != expected_requests
        ):
            raise RuntimeError(
                "durable capture kind mismatch: "
                f"requests={expected_requests}, counts={json.dumps(event_counts, sort_keys=True)}"
            )

        comparison = None
        if baseline is not None:
            comparison = {
                "throughput_ratio": (
                    gateway_result.requests_per_second / baseline.requests_per_second
                    if baseline.requests_per_second
                    else None
                ),
                "p95_latency_ratio": (
                    gateway_result.latency_ms_p95 / baseline.latency_ms_p95
                    if baseline.latency_ms_p95
                    else None
                ),
            }
        return {
            "schema_version": 2,
            "generated_at": datetime.now(UTC).isoformat(),
            "system": {
                "platform": platform.platform(),
                "python": platform.python_version(),
                "cpu_count": os.cpu_count(),
            },
            "config": {
                "duration_seconds": args.duration,
                "request_limit": args.requests,
                "warmup_seconds": args.warmup,
                "concurrency": args.concurrency,
                "load_model": "closed_loop",
                "max_in_flight": args.concurrency,
                "sessions": args.sessions,
                "payload_bytes": args.payload_bytes,
                "capture_level": "full",
            },
            "echo_baseline": asdict(baseline) if baseline is not None else None,
            "gateway": asdict(gateway_result),
            "comparison": comparison,
            "durable_capture": {
                "expected_requests": expected_requests,
                "expected_events": expected_events,
                "shutdown_drain_seconds": shutdown_drain_seconds,
                "events_per_drain_second": (
                    expected_events / shutdown_drain_seconds
                    if shutdown_drain_seconds
                    else None
                ),
                **capture,
                "event_counts": event_counts,
                "validated": True,
            },
        }
    finally:
        stop_process(serve_process, label="pChronicle serve")
        for label, process in reversed(processes):
            stop_process(process, label=label)
        for handle in handles:
            handle.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--duration", type=float, default=10.0)
    parser.add_argument("--warmup", type=float, default=1.0)
    parser.add_argument("--concurrency", type=int, default=16)
    parser.add_argument(
        "--requests",
        type=int,
        default=1024,
        help="maximum requests per measured phase; duration remains a time ceiling",
    )
    parser.add_argument(
        "--sessions",
        type=int,
        default=16,
        help="fixed capture-session cardinality (independent of concurrency)",
    )
    parser.add_argument("--payload-bytes", type=int, default=256)
    parser.add_argument("--min-rps", type=float, default=0.0)
    parser.add_argument("--skip-baseline", action="store_true")
    parser.add_argument("--keep-artifacts", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.duration <= 0 or args.warmup < 0:
        parser.error("duration must be positive and warmup must be non-negative")
    if args.concurrency <= 0 or args.sessions <= 0 or args.requests <= 0:
        parser.error("concurrency, sessions, and requests must be positive")
    if args.payload_bytes < 0 or args.min_rps < 0:
        parser.error("payload-bytes and min-rps cannot be negative")
    return args


def interrupt_on_sigterm(_signum: int, _frame: object) -> None:
    raise KeyboardInterrupt


def main() -> None:
    args = parse_args()
    signal.signal(signal.SIGTERM, interrupt_on_sigterm)
    pchronicle = resolve_binary(
        "PERSISTING_PCHRONICLE_BIN",
        REPO_ROOT / "target" / "release" / "pchronicle",
        "cargo build --release --locked -p persisting-pchronicle-cli --bin pchronicle",
    )
    for subcommand in ["echo", "serve", "query"]:
        require_subcommand(
            pchronicle,
            subcommand,
            "cargo build --release --locked -p persisting-pchronicle-cli --bin pchronicle",
        )
    work_dir = Path(tempfile.mkdtemp(prefix="persisting-gateway-benchmark."))
    success = False
    try:
        result = benchmark(args, work_dir, pchronicle)
        output = args.output or SCENARIO_DIR / "results" / "latest.json"
        output = output.expanduser().resolve()
        output.parent.mkdir(parents=True, exist_ok=True)
        temporary = output.with_suffix(f"{output.suffix}.tmp")
        temporary.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        temporary.replace(output)
        gateway = result["gateway"]
        baseline = result["echo_baseline"]
        print(
            "Gateway: "
            f"{gateway['requests_per_second']:.2f} req/s, "
            f"mean={gateway['latency_ms_mean']:.2f} ms, "
            f"p50={gateway['latency_ms_p50']:.2f} ms, "
            f"p95={gateway['latency_ms_p95']:.2f} ms, "
            f"p99={gateway['latency_ms_p99']:.2f} ms, "
            f"requests={gateway['requests']}"
        )
        print(
            "Load: closed-loop, "
            f"max_in_flight={args.concurrency}, "
            f"estimated_mean_in_flight={gateway['estimated_mean_in_flight']:.2f}"
        )
        if baseline is not None:
            print(
                "Echo baseline: "
                f"{baseline['requests_per_second']:.2f} req/s, "
                f"p95={baseline['latency_ms_p95']:.2f} ms"
            )
        print(f"Durable capture: {json.dumps(result['durable_capture'], sort_keys=True)}")
        capture = result["durable_capture"]
        print(
            "Capture drain: "
            f"{capture['shutdown_drain_seconds']:.2f} s, "
            f"{capture['events_per_drain_second']:.2f} events/s"
        )
        print(f"Result: {output}")
        success = True
    finally:
        if args.keep_artifacts or not success:
            print(f"Gateway benchmark artifacts: {work_dir}", file=sys.stderr)
        else:
            shutil.rmtree(work_dir)


if __name__ == "__main__":
    main()
