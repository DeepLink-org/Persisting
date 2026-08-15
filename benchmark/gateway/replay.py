#!/usr/bin/env python3
"""Replay the example trajectory data through Gateway into a reviewable bundle."""

from __future__ import annotations

import argparse
import copy
import http.client
import json
import os
import re
import signal
import subprocess
import sys
import time
import urllib.parse
from collections import defaultdict
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, TextIO

SCENARIO_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCENARIO_DIR.parents[1]
sys.path.insert(0, str(REPO_ROOT / "tests" / "regression"))

from gateway_harness import (  # noqa: E402
    append_jsonl,
    require_subcommand,
    resolve_binary,
    stop_process,
    wait_http,
    wait_logged_url,
    without_proxy_environment,
)


@dataclass(frozen=True)
class ReplayCase:
    case_id: str
    source_file: str
    source_format: str
    source_record_id: str
    session_id: str
    model: str
    messages: list[dict[str, Any]]
    reference_response: Any
    source_record: dict[str, Any]


def source_label(path: Path) -> str:
    try:
        return str(path.relative_to(REPO_ROOT))
    except ValueError:
        return str(path)


def safe_id(value: object) -> str:
    normalized = re.sub(r"[^A-Za-z0-9_.-]+", "-", str(value)).strip("-.")
    return normalized or "unknown"


def message_text(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts: list[str] = []
        for part in content:
            if isinstance(part, str):
                parts.append(part)
            elif isinstance(part, dict):
                text = part.get("text", part.get("input_text"))
                if isinstance(text, str):
                    parts.append(text)
        return "".join(parts)
    if isinstance(content, dict):
        text = content.get("text")
        return text if isinstance(text, str) else ""
    return ""


def last_user_text(messages: list[dict[str, Any]]) -> str:
    for message in reversed(messages):
        if message.get("role") == "user":
            return message_text(message.get("content"))
    return ""


def openai_cases(path: Path, document: list[Any]) -> list[ReplayCase]:
    cases: list[ReplayCase] = []
    for index, row in enumerate(document):
        if not isinstance(row, dict) or not isinstance(row.get("messages"), list):
            raise ValueError(f"invalid OpenAI Messages row {index} in {path}")
        record_id = str(row.get("id", index))
        session = safe_id(row.get("session_id", record_id))
        model = str(row.get("agent_model", "example-openai-model"))
        cases.append(
            ReplayCase(
                case_id=f"openai-{safe_id(record_id)}",
                source_file=source_label(path),
                source_format="openai-messages",
                source_record_id=record_id,
                session_id=f"gateway-replay-openai-{session}",
                model=model,
                messages=copy.deepcopy(row["messages"]),
                reference_response=copy.deepcopy(row.get("response")),
                source_record=copy.deepcopy(row),
            )
        )
    return cases


def tool_call_message(tool_calls: Any) -> list[dict[str, Any]]:
    if not isinstance(tool_calls, list):
        return []
    converted: list[dict[str, Any]] = []
    for index, call in enumerate(tool_calls):
        if not isinstance(call, dict):
            continue
        call_id = str(call.get("tool_call_id", call.get("id", f"call-{index}")))
        name = str(call.get("function_name", call.get("name", "unknown_tool")))
        arguments = call.get("arguments", call.get("input", {}))
        converted.append(
            {
                "id": call_id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": json.dumps(arguments, ensure_ascii=False, sort_keys=True),
                },
            }
        )
    return converted


def append_tool_results(history: list[dict[str, Any]], tool_calls: Any, observations: Any) -> None:
    if isinstance(tool_calls, list):
        for index, call in enumerate(tool_calls):
            if not isinstance(call, dict) or "result" not in call:
                continue
            call_id = str(call.get("tool_call_id", call.get("id", f"call-{index}")))
            history.append(
                {
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": json.dumps(call["result"], ensure_ascii=False, sort_keys=True),
                }
            )
    if isinstance(observations, list):
        for index, observation in enumerate(observations):
            if not isinstance(observation, dict):
                continue
            call_id = str(observation.get("tool_use_id", f"observation-{index}"))
            history.append(
                {
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": message_text(observation.get("content")),
                }
            )


def atif_cases(path: Path, document: dict[str, Any]) -> list[ReplayCase]:
    session = safe_id(document.get("session_id", document.get("trajectory_id", path.stem)))
    agent = document.get("agent") if isinstance(document.get("agent"), dict) else {}
    model = str(agent.get("model_name", "example-atif-model"))
    history: list[dict[str, Any]] = []
    cases: list[ReplayCase] = []
    for index, step in enumerate(document.get("steps", [])):
        if not isinstance(step, dict):
            raise ValueError(f"invalid ATIF step {index} in {path}")
        source = step.get("source")
        message = message_text(step.get("message"))
        if source == "user":
            history.append({"role": "user", "content": message})
            continue
        if source != "agent":
            continue
        step_id = str(step.get("step_id", index))
        cases.append(
            ReplayCase(
                case_id=f"atif-{session}-step-{safe_id(step_id)}",
                source_file=source_label(path),
                source_format="atif",
                source_record_id=step_id,
                session_id=f"gateway-replay-atif-{session}",
                model=str(step.get("model_name", model)),
                messages=copy.deepcopy(history),
                reference_response={
                    "message": step.get("message"),
                    "tool_calls": copy.deepcopy(step.get("tool_calls", [])),
                },
                source_record=copy.deepcopy(step),
            )
        )
        assistant: dict[str, Any] = {"role": "assistant", "content": message}
        converted_calls = tool_call_message(step.get("tool_calls"))
        if converted_calls:
            assistant["tool_calls"] = converted_calls
        history.append(assistant)
        append_tool_results(history, step.get("tool_calls"), None)
    return cases


def actf_cases(path: Path, document: dict[str, Any]) -> list[ReplayCase]:
    task_id = safe_id(document.get("task_id", path.stem))
    attempts = document.get("attempts")
    if not isinstance(attempts, dict):
        raise ValueError(f"ACTF attempts must be an object in {path}")
    cases: list[ReplayCase] = []
    for attempt_id, attempt in sorted(attempts.items(), key=lambda item: str(item[0])):
        if not isinstance(attempt, dict):
            raise ValueError(f"invalid ACTF attempt {attempt_id} in {path}")
        trajectory = attempt.get("trajectory")
        if not isinstance(trajectory, dict) or not isinstance(trajectory.get("steps"), list):
            raise ValueError(f"ACTF attempt {attempt_id} has no trajectory steps in {path}")
        session = f"gateway-replay-actf-{task_id}-attempt-{safe_id(attempt_id)}"
        history: list[dict[str, Any]] = []
        current_system: str | None = None
        for index, step in enumerate(trajectory["steps"]):
            if not isinstance(step, dict):
                raise ValueError(f"invalid ACTF step {index} in {path}")
            system_prompt = message_text(step.get("system_prompt"))
            if system_prompt and system_prompt != current_system:
                history.append({"role": "system", "content": system_prompt})
                current_system = system_prompt
            user_content = message_text(step.get("user_content"))
            if user_content:
                history.append({"role": "user", "content": user_content})
            assistant_content = step.get("assistant_content")
            if not isinstance(assistant_content, dict):
                assistant_content = {"content": message_text(assistant_content)}
            step_id = str(step.get("step_id", index))
            cases.append(
                ReplayCase(
                    case_id=(
                        f"actf-{task_id}-attempt-{safe_id(attempt_id)}-step-{safe_id(step_id)}"
                    ),
                    source_file=source_label(path),
                    source_format="actf",
                    source_record_id=f"attempt-{attempt_id}/step-{step_id}",
                    session_id=session,
                    model="example-actf-model",
                    messages=copy.deepcopy(history),
                    reference_response=copy.deepcopy(assistant_content),
                    source_record=copy.deepcopy(step),
                )
            )
            assistant: dict[str, Any] = {
                "role": "assistant",
                "content": message_text(assistant_content.get("content")),
            }
            converted_calls = tool_call_message(assistant_content.get("tool_calls"))
            if converted_calls:
                assistant["tool_calls"] = converted_calls
            history.append(assistant)
            append_tool_results(history, None, step.get("observation"))
    return cases


def load_cases(data_dir: Path) -> list[ReplayCase]:
    cases: list[ReplayCase] = []
    json_paths = sorted(data_dir.rglob("*.json"))
    if not json_paths:
        raise ValueError(f"no JSON example data found under {data_dir}")
    for path in json_paths:
        document = json.loads(path.read_text(encoding="utf-8"))
        if isinstance(document, list):
            cases.extend(openai_cases(path, document))
        elif isinstance(document, dict) and str(document.get("schema_version", "")).startswith(
            "ATIF-"
        ):
            cases.extend(atif_cases(path, document))
        elif isinstance(document, dict) and "attempts" in document:
            cases.extend(actf_cases(path, document))
        else:
            raise ValueError(f"unsupported example JSON format: {path}")
    if not cases:
        raise ValueError(f"example data produced no replayable Gateway calls: {data_dir}")
    case_ids = [case.case_id for case in cases]
    if len(case_ids) != len(set(case_ids)):
        raise ValueError("example data produced duplicate replay case IDs")
    return cases


def write_configs(run_dir: Path, echo_url: str, cases: list[ReplayCase]) -> tuple[Path, Path]:
    warehouse = run_dir / "warehouse.toml"
    gateway = run_dir / "gateway.toml"
    warehouse.write_text(
        'default_dataset = "captures"\n\n'
        '[[datasets]]\nname = "captures"\n'
        f"uri = {json.dumps(str(run_dir / 'dataset'))}\n",
        encoding="utf-8",
    )
    routes = []
    for model in sorted({case.model for case in cases}):
        routes.append(
            "[[models]]\n"
            f"name = {json.dumps(model)}\n"
            'provider = "openai"\n'
            f"upstream = {json.dumps(echo_url + '/v1')}\n"
        )
    gateway.write_text(
        'listen = "127.0.0.1:0"\n'
        'admin_listen = "127.0.0.1:0"\n'
        'agent_id = "gateway-example-replay"\n'
        'capture_level = "full"\n\n'
        + "\n".join(routes),
        encoding="utf-8",
    )
    return warehouse, gateway


def send_case(gateway_url: str, case: ReplayCase) -> dict[str, Any]:
    parsed = urllib.parse.urlparse(gateway_url)
    if parsed.scheme != "http" or parsed.hostname is None or parsed.port is None:
        raise ValueError(f"Gateway must publish an explicit HTTP URL: {gateway_url}")
    request = {"model": case.model, "messages": case.messages, "stream": False}
    body = json.dumps(request, ensure_ascii=False, separators=(",", ":")).encode()
    headers = {
        "authorization": "Bearer gateway-example-replay",
        "content-type": "application/json",
        "x-persisting-session-id": case.session_id,
    }
    started = time.perf_counter()
    connection = http.client.HTTPConnection(parsed.hostname, parsed.port, timeout=15)
    try:
        connection.request("POST", "/v1/chat/completions", body=body, headers=headers)
        response = connection.getresponse()
        response_body = response.read()
    finally:
        connection.close()
    latency_ms = (time.perf_counter() - started) * 1000
    try:
        response_json = json.loads(response_body)
    except json.JSONDecodeError:
        response_json = None
    result = {
        "case_id": case.case_id,
        "session_id": case.session_id,
        "http_status": response.status,
        "latency_ms": latency_ms,
        "request": request,
        "response": response_json,
        "response_body": response_body.decode("utf-8", errors="replace"),
    }
    if response.status != 200 or not isinstance(response_json, dict):
        raise RuntimeError(
            f"Gateway replay failed for {case.case_id}: HTTP {response.status} "
            f"body={result['response_body'][:500]!r}"
        )
    return result


def query_events(pchronicle: Path, dataset: Path, output: Path) -> list[dict[str, Any]]:
    subprocess.run(
        [
            str(pchronicle),
            "query",
            str(dataset),
            (
                "SELECT seq, kind, session_id, model, call_id, payload_json "
                "FROM dataset.events ORDER BY session_id, seq"
            ),
            "--format",
            "jsonl",
            "--output",
            str(output),
        ],
        cwd=REPO_ROOT,
        check=True,
    )
    return [json.loads(line) for line in output.read_text(encoding="utf-8").splitlines()]


def response_text(result: dict[str, Any]) -> str:
    try:
        return str(result["response"]["choices"][0]["message"]["content"] or "")
    except (KeyError, IndexError, TypeError):
        return ""


def reference_text(reference: Any) -> str:
    if not isinstance(reference, dict):
        return message_text(reference)
    for field in ["content", "message", "reasoning_content"]:
        text = message_text(reference.get(field))
        if text:
            return text
    return ""


def captured_payload(event: dict[str, Any]) -> dict[str, Any]:
    payload = event.get("payload_json")
    if not isinstance(payload, str):
        raise ValueError(f"captured event has no payload_json: {event}")
    parsed = json.loads(payload)
    if not isinstance(parsed, dict):
        raise ValueError(f"captured payload is not an object: {event}")
    return parsed


def build_review(
    run_dir: Path,
    data_dir: Path,
    cases: list[ReplayCase],
    results: list[dict[str, Any]],
    events: list[dict[str, Any]],
) -> None:
    results_by_case = {result["case_id"]: result for result in results}
    requests_by_session: dict[str, list[dict[str, Any]]] = defaultdict(list)
    responses_by_session: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for event in events:
        kind = event.get("kind")
        if kind == "llm.request":
            requests_by_session[str(event.get("session_id"))].append(event)
        elif kind in {"llm.response", "llm.response.stream"}:
            responses_by_session[str(event.get("session_id"))].append(event)

    session_offsets: dict[str, int] = defaultdict(int)
    review_path = run_dir / "review.jsonl"
    checks_passed = 0
    markdown_rows: list[str] = []
    for case in cases:
        offset = session_offsets[case.session_id]
        session_offsets[case.session_id] += 1
        captured_requests = requests_by_session.get(case.session_id, [])
        captured_responses = responses_by_session.get(case.session_id, [])
        request_event = captured_requests[offset] if offset < len(captured_requests) else None
        response_event = captured_responses[offset] if offset < len(captured_responses) else None
        result = results_by_case[case.case_id]
        actual = response_text(result)
        expected_echo = last_user_text(case.messages)
        request_capture = None if request_event is None else captured_payload(request_event)
        response_capture = None if response_event is None else captured_payload(response_event)
        checks = {
            "http_200": result["http_status"] == 200,
            "echo_matches_last_user": actual == expected_echo,
            "captured_request": request_event is not None,
            "captured_response": response_event is not None,
            "capture_call_id_matches": (
                request_event is not None
                and response_event is not None
                and request_event.get("call_id") == response_event.get("call_id")
            ),
            "captured_request_wire_matches": (
                request_capture is not None
                and request_capture.get("payload", {}).get("http", {}).get("request_body")
                == result["request"]
            ),
            "captured_response_wire_matches": (
                response_capture is not None
                and response_capture.get("payload", {}).get("http", {}).get("response_body")
                == result["response"]
            ),
        }
        passed = all(checks.values())
        checks_passed += int(passed)
        append_jsonl(
            review_path,
            {
                "case": asdict(case),
                "gateway_result": result,
                "expected_echo_response": expected_echo,
                "captured_request": request_capture,
                "captured_response": response_capture,
                "checks": checks,
                "passed": passed,
            },
        )
        short_actual = actual.replace("\n", "\\n")[:64].replace("|", "\\|")
        short_reference = (
            reference_text(case.reference_response)
            .replace("\n", "\\n")[:64]
            .replace("|", "\\|")
        )
        markdown_rows.append(
            f"| `{case.case_id}` | `{case.source_format}` | {len(case.messages)} | "
            f"{'PASS' if passed else 'FAIL'} | {short_reference} | {short_actual} |"
        )

    expected_events = len(cases) * 2
    global_checks = {
        "exact_event_count": len(events) == expected_events,
        "only_request_response_events": all(
            event.get("kind") in {"llm.request", "llm.response"} for event in events
        ),
    }
    summary = {
        "schema_version": 1,
        "generated_at": datetime.now(UTC).isoformat(),
        "source_directory": str(data_dir),
        "output_directory": str(run_dir),
        "cases": len(cases),
        "expected_events": expected_events,
        "captured_events": len(events),
        "passed": checks_passed,
        "failed": len(cases) - checks_passed,
        "global_checks": global_checks,
    }
    (run_dir / "summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    (run_dir / "REVIEW.md").write_text(
        "# Gateway example-data replay review\n\n"
        f"Source: `{data_dir}`  \n"
        f"Cases: {len(cases)}  \n"
        f"Canonical events: {len(events)}/{expected_events}  \n"
        f"Automated checks: {checks_passed}/{len(cases)} passed\n\n"
        "The recorded source response is reference material only. The local Echo upstream "
        "deterministically returns the last user message, so this run reviews Gateway wire "
        "handling and capture fidelity rather than model-output equivalence.\n\n"
        "## Files\n\n"
        "- `inputs.jsonl`: source record plus the replayed Chat Completions messages.\n"
        "- `client-results.jsonl`: exact HTTP request and response seen by the driver.\n"
        "- `captured-events.jsonl`: canonical events exported from the Lance Dataset.\n"
        "- `review.jsonl`: one joined record per call, including parsed canonical payloads.\n"
        "- `dataset/`: durable Lance capture; `gateway-state/`: Gateway WAL/state.\n"
        "- `logs/`: Echo and Gateway process logs.\n\n"
        "## Calls\n\n"
        "| Case | Source format | Request messages | Checks | Reference response | Echo output |\n"
        "|---|---:|---:|---:|---|---|\n"
        + "\n".join(markdown_rows)
        + "\n",
        encoding="utf-8",
    )
    if checks_passed != len(cases) or not all(global_checks.values()):
        raise RuntimeError(
            f"replay review checks failed: cases={checks_passed}/{len(cases)} "
            f"global={global_checks}; see {review_path}"
        )


def run_replay(run_dir: Path, data_dir: Path, pchronicle: Path) -> None:
    cases = load_cases(data_dir)
    logs = run_dir / "logs"
    dataset = run_dir / "dataset"
    state = run_dir / "gateway-state"
    logs.mkdir()
    dataset.mkdir()
    state.mkdir()
    for case in cases:
        append_jsonl(run_dir / "inputs.jsonl", asdict(case))

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

        warehouse, gateway = write_configs(run_dir, echo_url, cases)
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

        results: list[dict[str, Any]] = []
        with without_proxy_environment():
            for case in cases:
                result = send_case(gateway_url, case)
                results.append(result)
                append_jsonl(run_dir / "client-results.jsonl", result)

        stop_process(serve_process, label="pChronicle serve", require_success=True)
        serve_process = None
        events = query_events(pchronicle, dataset, run_dir / "captured-events.jsonl")
        build_review(run_dir, data_dir, cases, results, events)
    finally:
        stop_process(serve_process, label="pChronicle serve")
        for label, process in reversed(processes):
            stop_process(process, label=label)
        for handle in handles:
            handle.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data", type=Path, default=REPO_ROOT / "examples" / "data")
    parser.add_argument(
        "--output",
        type=Path,
        default=SCENARIO_DIR / "results" / "replay-review",
        help="parent directory for timestamped review bundles",
    )
    return parser.parse_args()


def interrupt_on_sigterm(_signum: int, _frame: object) -> None:
    raise KeyboardInterrupt


def main() -> None:
    args = parse_args()
    signal.signal(signal.SIGTERM, interrupt_on_sigterm)
    data_dir = args.data.expanduser().resolve()
    if not data_dir.is_dir():
        raise ValueError(f"example data directory does not exist: {data_dir}")
    output_root = args.output.expanduser().resolve()
    output_root.mkdir(parents=True, exist_ok=True)
    run_id = datetime.now(UTC).strftime("%Y%m%dT%H%M%S.%fZ")
    run_dir = output_root / run_id
    run_dir.mkdir()
    (output_root / "latest.txt").write_text(str(run_dir) + "\n", encoding="utf-8")
    print(f"Gateway example replay review: {run_dir}", flush=True)

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
    run_replay(run_dir, data_dir, pchronicle)
    summary = json.loads((run_dir / "summary.json").read_text(encoding="utf-8"))
    print(
        f"PASS Gateway example replay: cases={summary['cases']} "
        f"events={summary['captured_events']} review={run_dir / 'REVIEW.md'}",
        flush=True,
    )


if __name__ == "__main__":
    main()
