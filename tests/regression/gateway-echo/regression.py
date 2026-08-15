#!/usr/bin/env python3
"""Black-box Gateway regression driven entirely from Python."""

from __future__ import annotations

import contextlib
import os
import shutil
import signal
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import TextIO

from compare_logs import compare_logs
from python_clients import run_all

SCENARIO_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCENARIO_DIR.parents[2]
sys.path.insert(0, str(SCENARIO_DIR.parent))

from gateway_harness import (  # noqa: E402
    require_subcommand,
    resolve_binary,
    stop_process,
    wait_http,
    wait_logged_url,
    without_proxy_environment,
)

EVENT_QUERY = (
    "SELECT seq, kind, session_id, model, call_id, payload_json "
    "FROM dataset.events ORDER BY session_id, seq"
)


def write_configs(
    work_dir: Path,
    *,
    echo_url: str,
    gateway_port: int,
    admin_port: int,
) -> tuple[Path, Path]:
    warehouse_config = work_dir / "warehouse.toml"
    gateway_config = work_dir / "gateway.toml"
    warehouse_config.write_text(
        f'''default_dataset = "captures"

[[datasets]]
name = "captures"
uri = "{work_dir / "dataset"}"
''',
        encoding="utf-8",
    )
    gateway_config.write_text(
        f'''listen = "127.0.0.1:{gateway_port}"
admin_listen = "127.0.0.1:{admin_port}"
agent_id = "gateway-echo-regression"
capture_level = "full"

[[models]]
name = "sdk-anthropic-model"
provider = "anthropic"
upstream = "{echo_url}/v1"
upstream_anthropic = "{echo_url}/v1"

[[models]]
name = "sdk-google-genai-model"
provider = "gemini"
upstream = "{echo_url}/v1beta"

[[models]]
name = "echo-openai"
provider = "openai"
upstream = "{echo_url}/v1"

[[models]]
name = "*"
forward = "echo-openai"
''',
        encoding="utf-8",
    )
    return warehouse_config, gateway_config


def run_regression(work_dir: Path, pchronicle: Path) -> None:
    logs_dir = work_dir / "logs"
    dataset_dir = work_dir / "dataset"
    state_dir = work_dir / "gateway-state"
    logs_dir.mkdir(parents=True)
    dataset_dir.mkdir()
    state_dir.mkdir()
    echo_process: subprocess.Popen[bytes] | None = None
    serve_process: subprocess.Popen[bytes] | None = None
    log_handles: list[TextIO] = []
    try:
        echo_log = (logs_dir / "echo.log").open("w", encoding="utf-8")
        log_handles.append(echo_log)
        echo_process = subprocess.Popen(
            [
                str(pchronicle),
                "echo",
                "--listen",
                "127.0.0.1:0",
                "--encoding",
                "plain",
            ],
            cwd=REPO_ROOT,
            stdout=echo_log,
            stderr=subprocess.STDOUT,
        )
        echo_url = wait_logged_url(
            logs_dir / "echo.log", "pChronicle Echo: ", echo_process, "pChronicle Echo"
        )
        wait_http(f"{echo_url}/health", echo_process, "pChronicle Echo")

        warehouse_config, gateway_config = write_configs(
            work_dir,
            echo_url=echo_url,
            gateway_port=0,
            admin_port=0,
        )

        serve_log = (logs_dir / "serve.log").open("w", encoding="utf-8")
        log_handles.append(serve_log)
        serve_process = subprocess.Popen(
            [
                str(pchronicle),
                "serve",
                "--config",
                str(warehouse_config),
                "--listen",
                "127.0.0.1:0",
                "--gateway",
                str(gateway_config),
                "--gateway-dataset",
                "captures",
                "--gateway-state",
                str(state_dir),
            ],
            cwd=REPO_ROOT,
            stdout=serve_log,
            stderr=subprocess.STDOUT,
        )
        warehouse_url = wait_logged_url(
            logs_dir / "serve.log",
            "pChronicle Warehouse: ",
            serve_process,
            "pChronicle Warehouse",
        )
        gateway_url = wait_logged_url(
            logs_dir / "serve.log",
            "pChronicle Gateway: ",
            serve_process,
            "pChronicle Gateway",
        )
        admin_url = wait_logged_url(
            logs_dir / "serve.log",
            "pChronicle Gateway admin: ",
            serve_process,
            "pChronicle Gateway admin",
        )
        wait_http(f"{warehouse_url}/api/v1/health", serve_process, "pChronicle Warehouse")
        wait_http(f"{admin_url}/admin/status", serve_process, "pChronicle Gateway admin")

        print("Running OpenAI, Anthropic, and Google Gen AI Python clients...", flush=True)
        with (logs_dir / "python-clients.log").open("w", encoding="utf-8") as client_log:
            with contextlib.redirect_stdout(client_log), contextlib.redirect_stderr(client_log):
                with without_proxy_environment():
                    run_all(gateway_url, logs_dir / "client-results.jsonl")

        # Graceful shutdown is part of the contract: pChronicle must drain the
        # Gateway capture queue before the durable event query runs.
        stop_process(serve_process, label="pChronicle serve", require_success=True)
        serve_process = None
        stop_process(echo_process, label="pChronicle Echo")
        echo_process = None

        events_log = logs_dir / "events.jsonl"
        subprocess.run(
            [
                str(pchronicle),
                "query",
                str(dataset_dir),
                EVENT_QUERY,
                "--format",
                "jsonl",
                "--output",
                str(events_log),
            ],
            cwd=REPO_ROOT,
            check=True,
        )
        client_count, event_count = compare_logs(
            logs_dir / "client-results.jsonl",
            events_log,
            logs_dir / "comparison.jsonl",
        )
        print(
            f"compared {client_count} SDK calls with {event_count} canonical events",
            flush=True,
        )
    finally:
        stop_process(serve_process, label="pChronicle serve")
        stop_process(echo_process, label="pChronicle Echo")
        for handle in log_handles:
            handle.close()


def interrupt_on_sigterm(_signum: int, _frame: object) -> None:
    raise KeyboardInterrupt


def main() -> None:
    signal.signal(signal.SIGTERM, interrupt_on_sigterm)
    pchronicle = resolve_binary(
        "PERSISTING_PCHRONICLE_BIN",
        REPO_ROOT / "target/release/pchronicle",
        "cargo build --release --locked -p persisting-pchronicle-cli",
    )
    require_subcommand(
        pchronicle,
        "echo",
        "cargo build --release --locked -p persisting-pchronicle-cli",
    )
    work_dir = Path(tempfile.mkdtemp(prefix="persisting-gateway-echo-regression."))
    success = False
    try:
        run_regression(work_dir, pchronicle)
        success = True
    finally:
        keep_artifacts = os.environ.get("PERSISTING_KEEP_TEST_ARTIFACTS") == "1"
        if keep_artifacts or not success:
            print(f"Gateway Echo regression artifacts: {work_dir}", file=sys.stderr)
        else:
            shutil.rmtree(work_dir)

    print("PASS pchronicle echo: Python SDK protocols and durable capture logs match")


if __name__ == "__main__":
    main()
