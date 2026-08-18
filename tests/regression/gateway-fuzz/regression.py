#!/usr/bin/env python3
"""Long-running randomized Gateway → Echo → pChronicle persistence fuzz test."""

from __future__ import annotations

import base64
import concurrent.futures
import hashlib
import http.client
import json
import os
import random
import secrets
import shutil
import signal
import socket
import string
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, TextIO

import anthropic
import openai
from anthropic import Anthropic
from google import genai
from google.genai import types
from openai import OpenAI

SCENARIO_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCENARIO_DIR.parents[2]
sys.path.insert(0, str(SCENARIO_DIR.parent))

from gateway_harness import (  # noqa: E402
    append_jsonl,
    require_subcommand,
    resolve_binary,
    stop_process,
    wait_http,
    wait_logged_url,
    without_proxy_environment,
)

THREAD_CLIENTS = threading.local()
FUZZ_AGENT_ID = "gateway-random-fuzz"
QUERY_SOURCE_BATCH_SIZE = 32
FUZZ_SUITES = {"formats", "forwarding", "storage", "network-policy"}
FORMAT_CONTRACT_SESSIONS = (
    "format-contract-tool",
    "format-contract-reasoning",
    "format-contract-multimodal",
    "format-contract-responses-tool",
    "format-contract-error",
)
ALPHABET = string.ascii_letters + string.digits + " \n\r\t\\\"'{}[],:/" + "中文日本語한국어🙂🚀éΩ"


@dataclass(frozen=True)
class FuzzCase:
    index: int
    case_seed: int
    session_id: str
    client: str
    protocol: str
    model: str
    input: str
    encoding: str
    streaming: bool
    provider: str
    response_kind: str
    upstream_path: str
    forward_to: str | None


def env_int(name: str, default: int, *, minimum: int = 1) -> int:
    raw = os.environ.get(name)
    value = default if raw is None else int(raw)
    if value < minimum:
        raise ValueError(f"{name} must be >= {minimum}, got {value}")
    return value


def resolve_pchronicle() -> Path:
    build_hint = "cargo build --release --locked -p persisting-pchronicle-cli"
    path = resolve_binary(
        "PERSISTING_PCHRONICLE_BIN",
        REPO_ROOT / "target/release/pchronicle",
        build_hint,
    )
    require_subcommand(path, "echo", build_hint)
    return path


def random_text(rng: random.Random, max_chars: int) -> str:
    bucket = rng.random()
    if bucket < 0.65:
        length = rng.randint(1, min(128, max_chars))
    elif bucket < 0.93:
        length = rng.randint(129, min(2048, max_chars)) if max_chars >= 129 else max_chars
    else:
        floor = min(2049, max_chars)
        length = rng.randint(floor, max_chars)
    return "".join(rng.choice(ALPHABET) for _ in range(length))


def make_case(index: int, master: random.Random, max_chars: int, suite: str) -> FuzzCase:
    case_seed = master.getrandbits(64)
    rng = random.Random(case_seed)
    protocols = (
        ["chat_completions", "responses"]
        if suite == "forwarding"
        else ["chat_completions", "responses", "messages", "gemini"]
    )
    protocol = (
        protocols[index % len(protocols)] if index < len(protocols) * 4 else rng.choice(protocols)
    )
    matrix_round = index // len(protocols)
    streaming = protocol != "gemini" and (
        bool(matrix_round & 1) if index < len(protocols) * 4 else rng.choice([True, False])
    )
    encoding = (
        ("base64" if matrix_round & 2 else "plain")
        if index < len(protocols) * 4
        else rng.choice(["plain", "base64"])
    )
    session_id = f"fuzz-{index:08d}-{case_seed:016x}"
    if protocol == "chat_completions":
        client, model, provider = "openai", f"fuzz-openai-{rng.randrange(16)}", "openai"
        upstream_path = "/v1/chat/completions"
        forward_to = "echo-openai" if suite == "forwarding" else None
    elif protocol == "responses":
        client, model, provider = "openai", f"fuzz-responses-{rng.randrange(16)}", "openai"
        upstream_path = "/v1/chat/completions"
        forward_to = "echo-openai" if suite == "forwarding" else None
    elif protocol == "messages":
        client, model, provider = "anthropic", "fuzz-anthropic", "anthropic"
        upstream_path, forward_to = "/v1/messages", None
    else:
        client, model, provider = "google-genai", "fuzz-gemini", "gemini"
        upstream_path, forward_to = "/v1beta/models/fuzz-gemini:generateContent", None
    return FuzzCase(
        index=index,
        case_seed=case_seed,
        session_id=session_id,
        client=client,
        protocol=protocol,
        model=model,
        input=random_text(rng, max_chars),
        encoding=encoding,
        streaming=streaming,
        provider=provider,
        response_kind="llm.response.stream" if streaming else "llm.response",
        upstream_path=upstream_path,
        forward_to=forward_to,
    )


def openai_client(gateway: str) -> OpenAI:
    client = getattr(THREAD_CLIENTS, "openai", None)
    if client is None:
        client = OpenAI(api_key="fuzz", base_url=f"{gateway}/v1", max_retries=0)
        THREAD_CLIENTS.openai = client
    return client


def anthropic_client(gateway: str) -> Anthropic:
    client = getattr(THREAD_CLIENTS, "anthropic", None)
    if client is None:
        client = Anthropic(api_key="fuzz", base_url=gateway, max_retries=0)
        THREAD_CLIENTS.anthropic = client
    return client


def execute_case(case: FuzzCase, gateway: str) -> dict[str, Any]:
    headers = {
        "x-persisting-session-id": case.session_id,
        "x-persisting-echo-encoding": case.encoding,
    }
    if case.protocol == "chat_completions":
        client = openai_client(gateway)
        response = client.chat.completions.create(
            model=case.model,
            messages=[{"role": "user", "content": case.input}],
            stream=case.streaming,
            extra_headers=headers,
        )
        if case.streaming:
            chunks: list[str] = []
            response_model = None
            for chunk in response:
                response_model = chunk.model or response_model
                if chunk.choices:
                    chunks.append(chunk.choices[0].delta.content or "")
            output = "".join(chunks)
        else:
            output = response.choices[0].message.content or ""
            response_model = response.model
    elif case.protocol == "responses":
        client = openai_client(gateway)
        if case.streaming:
            chunks: list[str] = []
            with client.responses.stream(
                model=case.model,
                input=case.input,
                extra_headers=headers,
            ) as stream:
                for event in stream:
                    if event.type == "response.output_text.delta":
                        chunks.append(event.delta)
                final_response = stream.get_final_response()
            output = "".join(chunks)
            response_model = final_response.model
        else:
            response = client.responses.create(
                model=case.model,
                input=case.input,
                extra_headers=headers,
            )
            output = response.output_text
            response_model = response.model
    elif case.protocol == "messages":
        client = anthropic_client(gateway)
        if case.streaming:
            with client.messages.stream(
                model=case.model,
                max_tokens=64,
                messages=[{"role": "user", "content": case.input}],
                extra_headers=headers,
            ) as stream:
                output = "".join(stream.text_stream)
                response_model = stream.get_final_message().model
        else:
            response = client.messages.create(
                model=case.model,
                max_tokens=64,
                messages=[{"role": "user", "content": case.input}],
                extra_headers=headers,
            )
            output = "".join(block.text for block in response.content if block.type == "text")
            response_model = response.model
    else:
        client = genai.Client(
            api_key="fuzz",
            http_options=types.HttpOptions(
                base_url=gateway,
                api_version="v1beta",
                headers=headers,
            ),
        )
        try:
            response = client.chats.create(model=case.model).send_message(case.input)
            output = response.text or ""
            response_model = response.model_version
        finally:
            client.close()

    expected = (
        case.input if case.encoding == "plain" else base64.b64encode(case.input.encode()).decode()
    )
    if output != expected:
        raise AssertionError(
            f"case {case.index} seed={case.case_seed}: client output mismatch "
            f"{text_difference(expected, output)}"
        )
    expected_response_model = (
        case.forward_to
        if case.protocol == "chat_completions" and case.forward_to is not None
        else case.model
    )
    if response_model != expected_response_model:
        raise AssertionError(
            f"case {case.index} seed={case.case_seed}: response model mismatch "
            f"expected={expected_response_model!r} actual={response_model!r}"
        )
    return {
        "index": case.index,
        "session_id": case.session_id,
        "output_chars": len(output),
        "output_sha256": hashlib.sha256(output.encode("utf-8")).hexdigest(),
        "response_model": response_model,
        "sdk_version": {
            "openai": openai.__version__,
            "anthropic": anthropic.__version__,
            "google-genai": genai.__version__,
        }[case.client],
    }


def text_difference(expected: str, actual: str) -> str:
    """Return a bounded, reproducible diagnostic for a large text mismatch."""
    common = min(len(expected), len(actual))
    offset = next((index for index in range(common) if expected[index] != actual[index]), common)
    expected_char = expected[offset] if offset < len(expected) else None
    actual_char = actual[offset] if offset < len(actual) else None
    start = max(0, offset - 32)
    end = offset + 33

    def digest(value: str) -> str:
        return hashlib.sha256(value.encode("utf-8")).hexdigest()

    def codepoint(value: str | None) -> str:
        return "EOF" if value is None else f"U+{ord(value):04X} {value!r}"

    return (
        f"first_difference={offset} "
        f"expected_char={codepoint(expected_char)} actual_char={codepoint(actual_char)} "
        f"expected_chars={len(expected)} actual_chars={len(actual)} "
        f"expected_sha256={digest(expected)} actual_sha256={digest(actual)} "
        f"expected_context={expected[start:end]!r} actual_context={actual[start:end]!r}"
    )


def parts_text(parts: list[dict[str, Any]]) -> str:
    return "".join(part.get("text", "") for part in parts if part.get("type") == "text")


def canonical_request_text(payload: dict[str, Any]) -> str:
    return "".join(
        parts_text(message.get("parts", []))
        for message in payload["llm_request"]["request"]["messages"]
        if message.get("role") == "user"
    )


def canonical_response_text(payload: dict[str, Any]) -> str:
    return "".join(
        parts_text(candidate.get("message", {}).get("parts", []))
        for candidate in payload["llm_response"]["response"].get("candidates", [])
    )


def load_jsonl_by_session(path: Path) -> dict[str, dict[str, Any]]:
    records: dict[str, dict[str, Any]] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        record = json.loads(line)
        session_id = record["session_id"]
        if session_id in records:
            raise AssertionError(f"duplicate session {session_id!r} in {path}")
        records[session_id] = record
    return records


def is_draft_payload(payload: dict[str, Any]) -> bool:
    return bool(
        payload.get("draft")
        or payload.get("llm_response", {}).get("draft")
        or payload.get("llm_response", {}).get("response", {}).get("draft")
    )


def compare_events(
    cases_path: Path,
    results_path: Path,
    events_path: Path,
    comparison_path: Path,
) -> tuple[int, int]:
    expected = load_jsonl_by_session(cases_path)
    results = load_jsonl_by_session(results_path)
    if set(results) != set(expected):
        raise AssertionError(
            "case/result session mismatch: "
            f"missing={sorted(set(expected) - set(results))[:10]} "
            f"unexpected={sorted(set(results) - set(expected))[:10]}"
        )
    states = {
        session_id: {
            "request_count": 0,
            "terminal_count": 0,
            "draft_count": 0,
            "cancel_count": 0,
            "call_id": None,
            "last_seq": None,
            "kinds": [],
        }
        for session_id in expected
    }
    failures: list[dict[str, Any]] = []
    event_count = 0
    with events_path.open(encoding="utf-8") as events:
        for line in events:
            event_count += 1
            event = json.loads(line)
            session_id = event["session_id"]
            case = expected.get(session_id)
            if case is None:
                failures.append({"session_id": session_id, "failure": "unexpected session"})
                continue
            payload = json.loads(event["payload_json"])["payload"]
            state = states[session_id]
            seq = event["seq"]
            if state["last_seq"] is not None and seq <= state["last_seq"]:
                failures.append(
                    {
                        "session_id": session_id,
                        "failure": "non-increasing seq",
                        "previous": state["last_seq"],
                        "actual": seq,
                    }
                )
            state["last_seq"] = seq
            state["kinds"].append(event["kind"])
            call_id = event.get("call_id")
            if not call_id:
                failures.append(
                    {"session_id": session_id, "kind": event["kind"], "failure": "missing call_id"}
                )
            elif state["call_id"] is None:
                state["call_id"] = call_id
            elif state["call_id"] != call_id:
                failures.append(
                    {
                        "session_id": session_id,
                        "kind": event["kind"],
                        "failure": "call_id changed within lifecycle",
                        "expected": state["call_id"],
                        "actual": call_id,
                    }
                )
            if event["kind"] == "llm.request":
                checks = {
                    "model": event["model"] == case["model"],
                    "protocol": payload["protocol"] == case["protocol"],
                    "provider": payload["provider"] == case["provider"],
                    "input": canonical_request_text(payload) == case["input"],
                    "forward_to": payload.get("forward_to") == case["forward_to"],
                }
                state["request_count"] += 1
            elif event["kind"] == "llm.call.cancelled":
                checks = {"unexpected_cancel": False}
                state["cancel_count"] += 1
            elif event["kind"] in {
                "llm.response",
                "llm.response.stream",
            } and is_draft_payload(payload):
                checks = {"draft_after_request": state["request_count"] == 1}
                state["draft_count"] += 1
            elif event["kind"] in {"llm.response", "llm.response.stream"}:
                expected_output = (
                    case["input"]
                    if case["encoding"] == "plain"
                    else base64.b64encode(case["input"].encode()).decode()
                )
                checks = {
                    "kind": event["kind"] == case["response_kind"],
                    "after_request": state["request_count"] == 1,
                    "output": canonical_response_text(payload) == expected_output,
                    "status": payload["status"] == 200,
                    "upstream_path": payload["http"]["url"].endswith(case["upstream_path"]),
                    "forward_to": payload.get("forward_to") == case["forward_to"],
                }
                state["terminal_count"] += 1
            else:
                checks = {"recognized_lifecycle_kind": False}
            if not all(checks.values()):
                failures.append({"session_id": session_id, "kind": event["kind"], "checks": checks})

    with comparison_path.open("w", encoding="utf-8") as comparisons:
        for session_id, state in states.items():
            matched = bool(
                state["request_count"] == 1
                and state["terminal_count"] == 1
                and state["cancel_count"] == 0
            )
            record = {"session_id": session_id, "matched": matched, **state}
            json.dump(record, comparisons, sort_keys=True)
            comparisons.write("\n")
            if not matched:
                failures.append({"session_id": session_id, "failure": "invalid lifecycle", **state})

    if failures:
        raise AssertionError(json.dumps(failures[:20], ensure_ascii=False, indent=2))
    return len(expected), event_count


def sql_string(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def query_captured_events(
    pchronicle: Path,
    dataset: Path,
    results_path: Path,
    events_path: Path,
) -> None:
    """Read owned sources in bounded groups so low macOS FD limits are sufficient."""
    session_ids = [
        json.loads(line)["session_id"]
        for line in results_path.read_text(encoding="utf-8").splitlines()
    ]
    query_session_events(pchronicle, dataset, session_ids, events_path)


def query_session_events(
    pchronicle: Path,
    dataset: Path,
    session_ids: list[str],
    events_path: Path,
) -> None:
    events_path.write_bytes(b"")
    for batch_index, start in enumerate(range(0, len(session_ids), QUERY_SOURCE_BATCH_SIZE)):
        batch = session_ids[start : start + QUERY_SOURCE_BATCH_SIZE]
        source_filter = ", ".join(
            sql_string(f"{FUZZ_AGENT_ID}/{session_id}/events.lance") for session_id in batch
        )
        part_path = events_path.with_name(f"{events_path.stem}-{batch_index:05d}.jsonl")
        try:
            query = subprocess.run(
                [
                    str(pchronicle),
                    "query",
                    str(dataset),
                    "SELECT seq, kind, session_id, model, call_id, payload_json "
                    f"FROM dataset.events WHERE _file_ IN ({source_filter}) "
                    "ORDER BY session_id, seq",
                    "--format",
                    "jsonl",
                    "--output",
                    str(part_path),
                    "--max-output-rows",
                    str(len(batch) * 64 + 100),
                    "--max-output-bytes",
                    str(1024 * 1024 * 1024),
                    "--timeout-seconds",
                    "300",
                ],
                cwd=REPO_ROOT,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )
            if query.returncode != 0:
                raise RuntimeError(
                    f"pChronicle query batch {batch_index} failed "
                    f"for {len(batch)} sources (status {query.returncode}):\n{query.stdout}"
                )
            with events_path.open("ab") as output, part_path.open("rb") as part:
                shutil.copyfileobj(part, output)
        finally:
            part_path.unlink(missing_ok=True)


def write_configs(
    work_dir: Path,
    echo_url: str,
    gateway_port: int,
    admin_port: int,
    suite: str,
) -> tuple[Path, Path]:
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
    forwarding_routes = (
        f'''[[models]]
name = "echo-openai"
provider = "openai"
upstream = "{echo_url}/v1"

[[models]]
name = "*"
forward = "echo-openai"
'''
        if suite == "forwarding"
        else f'''[[models]]
name = "fuzz-openai-*"
provider = "openai"
upstream = "{echo_url}/v1"

[[models]]
name = "fuzz-responses-*"
provider = "openai"
upstream = "{echo_url}/v1"
'''
    )
    gateway.write_text(
        f'''listen = "127.0.0.1:{gateway_port}"
admin_listen = "127.0.0.1:{admin_port}"
agent_id = "{FUZZ_AGENT_ID}"
capture_level = "full"

[[models]]
name = "fuzz-anthropic"
provider = "anthropic"
upstream = "{echo_url}/v1"
upstream_anthropic = "{echo_url}/v1"

[[models]]
name = "fuzz-gemini"
provider = "gemini"
upstream = "{echo_url}/v1beta"

{forwarding_routes}
''',
        encoding="utf-8",
    )
    return warehouse, gateway


def write_network_configs(
    work_dir: Path,
    name: str,
    echo_url: str,
    echo_port: int,
    gateway_port: int,
    admin_port: int,
    mode: str,
) -> tuple[Path, Path, Path, Path]:
    root = work_dir / name
    dataset = root / "dataset"
    state = root / "gateway-state"
    dataset.mkdir(parents=True)
    state.mkdir()
    warehouse = root / "warehouse.toml"
    gateway = root / "gateway.toml"
    warehouse.write_text(
        f'''default_dataset = "captures"

[[datasets]]
name = "captures"
uri = "{dataset}"
''',
        encoding="utf-8",
    )
    if mode == "allowlist":
        network = f"""[network]
mode = "allowlist"

[[network.rules]]
host = "127.0.0.1"
ports = [{echo_port}]
transports = ["http", "tcp_tunnel"]
"""
    else:
        network = """[network]
mode = "no-network"
"""
    gateway.write_text(
        f'''listen = "127.0.0.1:{gateway_port}"
admin_listen = "127.0.0.1:{admin_port}"
agent_id = "gateway-network-fuzz-{name}"
capture_level = "full"

{network}

[[models]]
name = "network-model"
provider = "openai"
upstream = "{echo_url}/v1"
''',
        encoding="utf-8",
    )
    return warehouse, gateway, dataset, state


def start_pchronicle_serve(
    pchronicle: Path,
    warehouse: Path,
    gateway: Path,
    state: Path,
    warehouse_port: int,
    log: TextIO,
) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [
            str(pchronicle),
            "serve",
            "--config",
            str(warehouse),
            "--listen",
            f"127.0.0.1:{warehouse_port}",
            "--gateway",
            str(gateway),
            "--gateway-dataset",
            "captures",
            "--gateway-state",
            str(state),
        ],
        cwd=REPO_ROOT,
        stdout=log,
        stderr=subprocess.STDOUT,
    )


def proxy_absolute_get(gateway_port: int, target: str, target_host: str) -> tuple[int, str]:
    connection = http.client.HTTPConnection("127.0.0.1", gateway_port, timeout=5)
    try:
        connection.request("GET", target, headers={"Host": target_host})
        response = connection.getresponse()
        return response.status, response.read().decode("utf-8", errors="replace")
    finally:
        connection.close()


def proxy_connect(gateway_port: int, authority: str) -> tuple[int, str]:
    with socket.create_connection(("127.0.0.1", gateway_port), timeout=5) as connection:
        connection.sendall(f"CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n".encode())
        response = bytearray()
        while b"\r\n\r\n" not in response and len(response) < 64 * 1024:
            chunk = connection.recv(4096)
            if not chunk:
                break
            response.extend(chunk)
    text = response.decode("utf-8", errors="replace")
    first_line = text.splitlines()[0] if text.splitlines() else ""
    fields = first_line.split()
    status = int(fields[1]) if len(fields) >= 2 and fields[1].isdigit() else 0
    return status, text


def relative_llm_request(gateway_port: int, session_id: str, input_text: str) -> tuple[int, str]:
    body = json.dumps(
        {
            "model": "network-model",
            "messages": [{"role": "user", "content": input_text}],
        },
        ensure_ascii=False,
    ).encode()
    connection = http.client.HTTPConnection("127.0.0.1", gateway_port, timeout=5)
    try:
        connection.request(
            "POST",
            "/v1/chat/completions",
            body=body,
            headers={
                "content-type": "application/json",
                "x-persisting-session-id": session_id,
            },
        )
        response = connection.getresponse()
        response_body = response.read().decode("utf-8", errors="replace")
        return response.status, response_body
    finally:
        connection.close()


def gateway_json_request(
    gateway: str,
    path: str,
    payload: dict[str, Any],
    *,
    session_id: str,
    mode: str,
) -> tuple[int, str, dict[str, str]]:
    parsed = urllib.parse.urlparse(gateway)
    assert parsed.hostname is not None and parsed.port is not None
    connection = http.client.HTTPConnection(parsed.hostname, parsed.port, timeout=10)
    try:
        connection.request(
            "POST",
            path,
            body=json.dumps(payload, ensure_ascii=False).encode("utf-8"),
            headers={
                "content-type": "application/json",
                "x-persisting-session-id": session_id,
                "x-persisting-echo-mode": mode,
            },
        )
        response = connection.getresponse()
        body = response.read().decode("utf-8", errors="strict")
        return response.status, body, {key.lower(): value for key, value in response.getheaders()}
    finally:
        connection.close()


def response_sse_events(body: str) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    for frame in body.replace("\r\n", "\n").split("\n\n"):
        data = "\n".join(
            line[5:].strip() for line in frame.splitlines() if line.startswith("data:")
        )
        if data and data != "[DONE]":
            events.append(json.loads(data))
    return events


def run_format_contract_cases(gateway: str, results_path: Path) -> int:
    """Exercise non-text response contracts that random text round-trips cannot cover."""
    cases = 0

    status, body, _ = gateway_json_request(
        gateway,
        "/v1/chat/completions",
        {"model": "fuzz-openai-0", "messages": [{"role": "user", "content": "tool"}]},
        session_id="format-contract-tool",
        mode="tool",
    )
    tool = json.loads(body)
    call = tool["choices"][0]["message"]["tool_calls"][0]
    if status != 200 or call["function"] != {
        "name": "weather",
        "arguments": '{"city":"Paris"}',
    }:
        raise AssertionError(
            f"Chat tool-call contract failed: status={status} body={body[:1000]!r}"
        )
    append_jsonl(results_path, {"case": "chat-tool", "status": status, "call": call})
    cases += 1

    status, body, _ = gateway_json_request(
        gateway,
        "/v1/chat/completions",
        {"model": "fuzz-openai-0", "messages": [{"role": "user", "content": "reason"}]},
        session_id="format-contract-reasoning",
        mode="reasoning",
    )
    reasoning = json.loads(body)
    message = reasoning["choices"][0]["message"]
    if (
        status != 200
        or message.get("reasoning_content") != "echo-reasoning"
        or message.get("content") != "reason"
    ):
        raise AssertionError(
            f"Chat reasoning contract failed: status={status} body={body[:1000]!r}"
        )
    append_jsonl(
        results_path,
        {"case": "chat-reasoning", "status": status, "reasoning": message["reasoning_content"]},
    )
    cases += 1

    status, body, _ = gateway_json_request(
        gateway,
        "/v1/chat/completions",
        {
            "model": "fuzz-openai-0",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "describe"},
                        {
                            "type": "image_url",
                            "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="},
                        },
                    ],
                }
            ],
        },
        session_id="format-contract-multimodal",
        mode="inspect",
    )
    multimodal = json.loads(body)
    content = multimodal["choices"][0]["message"]["content"]
    if status != 200 or content != "text=1 image=1":
        raise AssertionError(
            f"Chat multimodal contract failed: status={status} body={body[:1000]!r}"
        )
    append_jsonl(results_path, {"case": "chat-multimodal", "status": status, "summary": content})
    cases += 1

    status, body, _ = gateway_json_request(
        gateway,
        "/v1/responses",
        {
            "model": "fuzz-responses-0",
            "input": "tool",
            "stream": True,
            "tools": [
                {
                    "type": "function",
                    "name": "weather",
                    "description": "weather lookup",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}},
                }
            ],
        },
        session_id="format-contract-responses-tool",
        mode="tool",
    )
    response_events = response_sse_events(body)
    completed = next(
        (event for event in response_events if event.get("type") == "response.completed"),
        None,
    )
    output = None if completed is None else completed["response"].get("output")
    if (
        status != 200
        or not isinstance(output, list)
        or len(output) != 1
        or output[0].get("type") != "function_call"
        or output[0].get("name") != "weather"
    ):
        raise AssertionError(
            "Responses terminal output contract failed: "
            f"status={status} events={response_events[-5:]!r}"
        )
    append_jsonl(
        results_path,
        {"case": "responses-terminal-tool", "status": status, "output": output},
    )
    cases += 1

    status, body, _ = gateway_json_request(
        gateway,
        "/v1/chat/completions",
        {"model": "fuzz-openai-0", "messages": [{"role": "user", "content": "fail"}]},
        session_id="format-contract-error",
        mode="error",
    )
    error = json.loads(body).get("error", {})
    if status != 429 or error != {
        "message": "controlled echo failure",
        "type": "echo_controlled_error",
        "code": "echo_rate_limit",
    }:
        raise AssertionError(f"error-body contract failed: status={status} body={body[:1000]!r}")
    append_jsonl(results_path, {"case": "error-body", "status": status, "error": error})
    cases += 1
    return cases


def canonical_candidate_parts(payload: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        part
        for candidate in payload["llm_response"]["response"].get("candidates", [])
        for part in candidate.get("message", {}).get("parts", [])
    ]


def validate_durable_content_contracts(events_path: Path, comparison_path: Path) -> int:
    grouped: dict[str, list[dict[str, Any]]] = {
        session_id: [] for session_id in FORMAT_CONTRACT_SESSIONS
    }
    for line in events_path.read_text(encoding="utf-8").splitlines():
        event = json.loads(line)
        if event["session_id"] not in grouped:
            raise AssertionError(f"unexpected content-contract session {event['session_id']!r}")
        event["payload"] = json.loads(event["payload_json"])["payload"]
        grouped[event["session_id"]].append(event)

    comparisons: list[dict[str, Any]] = []
    failures: list[dict[str, Any]] = []
    for session_id, events in grouped.items():
        requests = [event for event in events if event["kind"] == "llm.request"]
        drafts = [
            event
            for event in events
            if event["kind"] in {"llm.response", "llm.response.stream"}
            and is_draft_payload(event["payload"])
        ]
        terminals = [
            event
            for event in events
            if event["kind"] in {"llm.response", "llm.response.stream"}
            and not is_draft_payload(event["payload"])
        ]
        cancellations = [event for event in events if event["kind"] == "llm.call.cancelled"]
        seqs = [event["seq"] for event in events]
        call_ids = {event.get("call_id") for event in events}
        checks: dict[str, bool] = {
            "one_request": len(requests) == 1,
            "one_terminal": len(terminals) == 1,
            "no_cancel": not cancellations,
            "ordered": all(left < right for left, right in zip(seqs, seqs[1:])),
            "stable_call_id": len(call_ids) == 1 and None not in call_ids,
        }
        if len(requests) == 1 and len(terminals) == 1:
            request = requests[0]["payload"]
            terminal = terminals[0]
            response = terminal["payload"]
            request_parts = [
                part
                for message in request["llm_request"]["request"].get("messages", [])
                for part in message.get("parts", [])
            ]
            response_parts = canonical_candidate_parts(response)
            if session_id == "format-contract-tool":
                checks["tool_call"] = any(
                    part.get("type") == "tool_call"
                    and part.get("id") == "call_echo_weather"
                    and part.get("name") == "weather"
                    and part.get("arguments") == {"city": "Paris"}
                    for part in response_parts
                )
            elif session_id == "format-contract-responses-tool":
                checks["stream_terminal"] = terminal["kind"] == "llm.response.stream"
                checks["tool_call"] = any(
                    part.get("type") == "tool_call"
                    and part.get("name") == "weather"
                    and part.get("arguments") == {"city": "Paris"}
                    for part in response_parts
                )
            elif session_id == "format-contract-reasoning":
                checks["reasoning"] = any(
                    part.get("type") == "reasoning" and part.get("text") == "echo-reasoning"
                    for part in response_parts
                )
                checks["visible_text"] = any(
                    part.get("type") == "text" and part.get("text") == "reason"
                    for part in response_parts
                )
            elif session_id == "format-contract-multimodal":
                checks["request_text"] = any(
                    part.get("type") == "text" and part.get("text") == "describe"
                    for part in request_parts
                )
                checks["request_image"] = any(
                    part.get("type") == "image"
                    and part.get("source", {}).get("type") == "data"
                    and part.get("source", {}).get("data") == "data:image/png;base64,iVBORw0KGgo="
                    for part in request_parts
                )
                checks["response_summary"] = canonical_response_text(response) == "text=1 image=1"
            elif session_id == "format-contract-error":
                checks["status"] = response.get("status") == 429
                checks["error_body"] = response.get("http", {}).get("response_body") == {
                    "error": {
                        "message": "controlled echo failure",
                        "type": "echo_controlled_error",
                        "code": "echo_rate_limit",
                    }
                }
        record = {
            "session_id": session_id,
            "matched": all(checks.values()),
            "checks": checks,
            "kinds": [event["kind"] for event in events],
            "draft_count": len(drafts),
        }
        comparisons.append(record)
        if not record["matched"]:
            failures.append(record)

    with comparison_path.open("w", encoding="utf-8") as output:
        for record in comparisons:
            output.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")
    if failures:
        raise AssertionError(json.dumps(failures, ensure_ascii=False, indent=2))
    return sum(len(events) for events in grouped.values())


def run_network_fuzz(work_dir: Path, pchronicle: Path, seed: int) -> tuple[int, int]:
    duration = env_int("PERSISTING_FUZZ_DURATION_SECONDS", 60)
    rate = env_int("PERSISTING_FUZZ_REQUESTS_PER_SECOND", 5)
    replay_raw = os.environ.get("PERSISTING_FUZZ_REPLAY_CASE")
    replay_index = None if replay_raw is None else int(replay_raw)
    if replay_index is not None and replay_index < 0:
        raise ValueError("PERSISTING_FUZZ_REPLAY_CASE must be >= 0")
    (work_dir / "run.json").write_text(
        json.dumps(
            {
                "suite": "network-policy",
                "seed": seed,
                "duration_seconds": duration,
                "requests_per_second": rate,
                "replay_case": replay_index,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )

    logs = work_dir / "logs"
    logs.mkdir()
    handles: list[TextIO] = []
    processes: list[tuple[str, subprocess.Popen[bytes]]] = []
    try:
        echo_log = (logs / "echo.log").open("w", encoding="utf-8")
        handles.append(echo_log)
        echo_process = subprocess.Popen(
            [str(pchronicle), "echo", "--listen", "127.0.0.1:0"],
            cwd=REPO_ROOT,
            stdout=echo_log,
            stderr=subprocess.STDOUT,
        )
        processes.append(("pChronicle Echo", echo_process))
        echo_url = wait_logged_url(
            logs / "echo.log", "pChronicle Echo: ", echo_process, "pChronicle Echo"
        )
        echo_port = urllib.parse.urlparse(echo_url).port
        assert echo_port is not None
        wait_http(f"{echo_url}/health", echo_process, "pChronicle Echo")

        allow = write_network_configs(work_dir, "allowlist", echo_url, echo_port, 0, 0, "allowlist")
        deny = write_network_configs(
            work_dir, "no-network", echo_url, echo_port, 0, 0, "no-network"
        )
        blocked_port = 1 if echo_port != 1 else 2
        listeners: dict[str, tuple[int, int]] = {}

        for name, config in [("allowlist", allow), ("no-network", deny)]:
            warehouse, gateway, _dataset, state = config
            serve_log = (logs / f"serve-{name}.log").open("w", encoding="utf-8")
            handles.append(serve_log)
            process = start_pchronicle_serve(pchronicle, warehouse, gateway, state, 0, serve_log)
            processes.append((f"pChronicle serve {name}", process))
            warehouse_url = wait_logged_url(
                logs / f"serve-{name}.log",
                "pChronicle Warehouse: ",
                process,
                f"pChronicle Warehouse {name}",
            )
            gateway_url = wait_logged_url(
                logs / f"serve-{name}.log",
                "pChronicle Gateway: ",
                process,
                f"pChronicle Gateway {name}",
            )
            admin_url = wait_logged_url(
                logs / f"serve-{name}.log",
                "pChronicle Gateway admin: ",
                process,
                f"pChronicle Gateway admin {name}",
            )
            gateway_port = urllib.parse.urlparse(gateway_url).port
            admin_port = urllib.parse.urlparse(admin_url).port
            assert gateway_port is not None and admin_port is not None
            listeners[name] = (gateway_port, admin_port)
            wait_http(
                f"{warehouse_url}/api/health",
                process,
                f"pChronicle Warehouse {name}",
            )
            wait_http(
                f"{admin_url}/admin/status",
                process,
                f"pChronicle Gateway {name}",
            )

        allow_gateway_port, _allow_admin_port = listeners["allowlist"]
        deny_gateway_port, _deny_admin_port = listeners["no-network"]

        case_kinds = [
            "allow-absolute",
            "allow-connect",
            "deny-hostname",
            "deny-port",
            "no-network-absolute",
            "no-network-connect",
            "allow-relative-llm",
            "no-network-relative-llm",
        ]
        cases_path = logs / "cases.jsonl"
        results_path = logs / "client-results.jsonl"
        master = random.Random(seed)
        deadline = time.monotonic() + duration
        interval = 1 / rate
        next_case = time.monotonic()
        next_progress = next_case + 10
        index = 0
        while time.monotonic() < deadline:
            if replay_index is not None and index > replay_index:
                break
            now = time.monotonic()
            if replay_index is None and now < next_case:
                time.sleep(min(0.05, next_case - now))
                continue
            case_seed = master.getrandbits(64)
            kind = (
                case_kinds[index]
                if index < len(case_kinds)
                else random.Random(case_seed).choice(case_kinds)
            )
            if replay_index is not None and index < replay_index:
                index += 1
                continue
            case = {"index": index, "case_seed": case_seed, "kind": kind}
            append_jsonl(cases_path, case)
            authority = f"127.0.0.1:{echo_port}"
            if kind == "allow-absolute":
                status, body = proxy_absolute_get(
                    allow_gateway_port, f"{echo_url}/health", authority
                )
                expected = 200
            elif kind == "allow-connect":
                status, body = proxy_connect(allow_gateway_port, authority)
                expected = 200
            elif kind == "deny-hostname":
                status, body = proxy_absolute_get(
                    allow_gateway_port,
                    f"http://localhost:{echo_port}/health",
                    f"localhost:{echo_port}",
                )
                expected = 403
            elif kind == "deny-port":
                status, body = proxy_absolute_get(
                    allow_gateway_port,
                    f"http://127.0.0.1:{blocked_port}/health",
                    f"127.0.0.1:{blocked_port}",
                )
                expected = 403
            elif kind == "no-network-absolute":
                status, body = proxy_absolute_get(
                    deny_gateway_port, f"{echo_url}/health", authority
                )
                expected = 403
            elif kind == "no-network-connect":
                status, body = proxy_connect(deny_gateway_port, authority)
                expected = 403
            else:
                input_text = f"network policy relative LLM {index} 中文🙂"
                gateway_port = (
                    allow_gateway_port if kind == "allow-relative-llm" else deny_gateway_port
                )
                status, body = relative_llm_request(
                    gateway_port, f"network-fuzz-{index:08d}", input_text
                )
                expected = 200
                if status == 200:
                    payload = json.loads(body)
                    output = payload["choices"][0]["message"]["content"]
                    if output != input_text:
                        raise AssertionError(
                            f"network case {index} relative LLM mismatch: "
                            f"{text_difference(input_text, output)}"
                        )
            expected_reason = {
                "deny-hostname": "not-in-allowlist",
                "deny-port": "port-not-allowed",
                "no-network-absolute": "no-network",
                "no-network-connect": "no-network",
            }.get(kind)
            if status != expected:
                raise AssertionError(
                    f"network case {index} seed={case_seed} kind={kind}: "
                    f"expected status {expected}, got {status}, body={body[:500]!r}"
                )
            if expected_reason is not None and expected_reason not in body:
                raise AssertionError(
                    f"network case {index} seed={case_seed} kind={kind}: "
                    f"expected denial reason {expected_reason!r}, body={body[:500]!r}"
                )
            if kind == "allow-absolute":
                health = json.loads(body)
                if health != {"status": "ok", "service": "pchronicle-echo"}:
                    raise AssertionError(f"network case {index} allow body mismatch: {health!r}")
            append_jsonl(
                results_path,
                {
                    **case,
                    "status": status,
                    "denial_reason": expected_reason,
                    "body_sha256": hashlib.sha256(body.encode()).hexdigest(),
                },
            )
            index += 1
            next_case += interval
            if now >= next_progress:
                print(
                    "Gateway network-policy fuzz progress "
                    f"elapsed={int(duration - max(0, deadline - now))}s "
                    f"completed={index}",
                    flush=True,
                )
                next_progress = now + 10

        for label, process in reversed(processes):
            stop_process(process, label=label, require_success=label.startswith("pChronicle serve"))
        processes.clear()
        return index, 0
    finally:
        for label, process in reversed(processes):
            stop_process(process, label=label)
        for handle in handles:
            handle.close()


def run_fuzz(work_dir: Path, pchronicle: Path, seed: int, suite: str) -> tuple[int, int]:
    duration = env_int("PERSISTING_FUZZ_DURATION_SECONDS", 60)
    concurrency = env_int("PERSISTING_FUZZ_CONCURRENCY", 8)
    rate = env_int("PERSISTING_FUZZ_REQUESTS_PER_SECOND", 5)
    max_chars = env_int("PERSISTING_FUZZ_MAX_MESSAGE_CHARS", 16_384)
    replay_raw = os.environ.get("PERSISTING_FUZZ_REPLAY_CASE")
    replay_index = None if replay_raw is None else int(replay_raw)
    if replay_index is not None and replay_index < 0:
        raise ValueError("PERSISTING_FUZZ_REPLAY_CASE must be >= 0")
    (work_dir / "run.json").write_text(
        json.dumps(
            {
                "seed": seed,
                "suite": suite,
                "duration_seconds": duration,
                "concurrency": concurrency,
                "requests_per_second": rate,
                "max_message_chars": max_chars,
                "replay_case": replay_index,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )

    logs = work_dir / "logs"
    dataset = work_dir / "dataset"
    state = work_dir / "gateway-state"
    logs.mkdir()
    dataset.mkdir()
    state.mkdir()
    echo_process: subprocess.Popen[bytes] | None = None
    serve_process: subprocess.Popen[bytes] | None = None
    handles: list[TextIO] = []
    try:
        echo_log = (logs / "echo.log").open("w", encoding="utf-8")
        handles.append(echo_log)
        echo_process = subprocess.Popen(
            [str(pchronicle), "echo", "--listen", "127.0.0.1:0"],
            cwd=REPO_ROOT,
            stdout=echo_log,
            stderr=subprocess.STDOUT,
        )
        echo_url = wait_logged_url(
            logs / "echo.log", "pChronicle Echo: ", echo_process, "pChronicle Echo"
        )
        wait_http(f"{echo_url}/health", echo_process, "pChronicle Echo")

        warehouse_config, gateway_config = write_configs(work_dir, echo_url, 0, 0, suite)

        serve_log = (logs / "serve.log").open("w", encoding="utf-8")
        handles.append(serve_log)
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
                str(state),
            ],
            cwd=REPO_ROOT,
            stdout=serve_log,
            stderr=subprocess.STDOUT,
        )
        warehouse_url = wait_logged_url(
            logs / "serve.log",
            "pChronicle Warehouse: ",
            serve_process,
            "pChronicle Warehouse",
        )
        gateway_url = wait_logged_url(
            logs / "serve.log",
            "pChronicle Gateway: ",
            serve_process,
            "pChronicle Gateway",
        )
        admin_url = wait_logged_url(
            logs / "serve.log",
            "pChronicle Gateway admin: ",
            serve_process,
            "pChronicle Gateway admin",
        )
        wait_http(
            f"{warehouse_url}/api/health",
            serve_process,
            "pChronicle Warehouse",
        )
        wait_http(
            f"{admin_url}/admin/status",
            serve_process,
            "pChronicle Gateway admin",
        )

        plan_path = logs / "cases.jsonl"
        results_path = logs / "client-results.jsonl"
        contract_results_path = logs / "format-contract-results.jsonl"
        failures_path = logs / "failures.jsonl"
        master = random.Random(seed)
        index = 0
        with without_proxy_environment():
            if replay_index is not None:
                for index in range(replay_index + 1):
                    case = make_case(index, master, max_chars, suite)
                append_jsonl(plan_path, asdict(case))
                try:
                    append_jsonl(results_path, execute_case(case, gateway_url))
                except Exception as error:
                    append_jsonl(failures_path, {**asdict(case), "error": str(error)})
                    raise
                index = 1
            else:
                deadline = time.monotonic() + duration
                started_at = time.monotonic()
                interval = 1 / rate
                next_submit = time.monotonic()
                next_progress = started_at + 10
                completed = 0
                pending: dict[concurrent.futures.Future[dict[str, Any]], FuzzCase] = {}
                with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as executor:
                    while time.monotonic() < deadline or pending:
                        now = time.monotonic()
                        while now < deadline and now >= next_submit and len(pending) < concurrency:
                            case = make_case(index, master, max_chars, suite)
                            append_jsonl(plan_path, asdict(case))
                            pending[executor.submit(execute_case, case, gateway_url)] = case
                            index += 1
                            next_submit += interval
                        if not pending:
                            time.sleep(min(0.05, max(0.0, next_submit - time.monotonic())))
                            continue
                        done, _ = concurrent.futures.wait(
                            pending,
                            timeout=0.05,
                            return_when=concurrent.futures.FIRST_COMPLETED,
                        )
                        for future in done:
                            case = pending.pop(future)
                            try:
                                append_jsonl(results_path, future.result())
                                completed += 1
                            except Exception as error:
                                append_jsonl(
                                    failures_path,
                                    {**asdict(case), "error": str(error)},
                                )
                                raise
                        now = time.monotonic()
                        if now >= next_progress:
                            print(
                                f"Gateway {suite} fuzz progress "
                                f"elapsed={int(now - started_at)}s "
                                f"submitted={index} completed={completed} pending={len(pending)}",
                                flush=True,
                            )
                            next_progress = now + 10

        contract_count = 0
        if suite in {"formats", "storage"}:
            contract_count = run_format_contract_cases(gateway_url, contract_results_path)

        stop_process(serve_process, label="pChronicle serve", require_success=True)
        serve_process = None
        stop_process(echo_process, label="pChronicle Echo")
        echo_process = None

        if suite == "formats":
            return index + contract_count, 0

        events_path = logs / "events.jsonl"
        query_captured_events(pchronicle, dataset, results_path, events_path)
        call_count, event_count = compare_events(
            plan_path, results_path, events_path, logs / "comparison.jsonl"
        )
        if suite == "storage":
            contract_events_path = logs / "content-contract-events.jsonl"
            query_session_events(
                pchronicle,
                dataset,
                list(FORMAT_CONTRACT_SESSIONS),
                contract_events_path,
            )
            contract_event_count = validate_durable_content_contracts(
                contract_events_path,
                logs / "content-contract-comparison.jsonl",
            )
            return call_count + contract_count, event_count + contract_event_count
        return call_count, event_count
    finally:
        stop_process(serve_process, label="pChronicle serve")
        stop_process(echo_process, label="pChronicle Echo")
        for handle in handles:
            handle.close()


def interrupt_on_sigterm(_signum: int, _frame: object) -> None:
    raise KeyboardInterrupt


def main() -> None:
    signal.signal(signal.SIGTERM, interrupt_on_sigterm)
    pchronicle = resolve_pchronicle()
    suite = os.environ.get("PERSISTING_GATEWAY_FUZZ_SUITE", "storage")
    if suite not in FUZZ_SUITES:
        raise ValueError(
            f"PERSISTING_GATEWAY_FUZZ_SUITE must be one of {sorted(FUZZ_SUITES)}, got {suite!r}"
        )
    seed = int(os.environ.get("PERSISTING_FUZZ_SEED", secrets.randbits(64)))
    work_dir = Path(tempfile.mkdtemp(prefix=f"persisting-gateway-{suite}-fuzz-{seed}."))
    success = False
    print(f"Gateway {suite} fuzz seed={seed} artifacts={work_dir}", flush=True)
    try:
        if suite == "network-policy":
            call_count, event_count = run_network_fuzz(work_dir, pchronicle, seed)
        else:
            call_count, event_count = run_fuzz(work_dir, pchronicle, seed, suite)
        success = True
    finally:
        keep = os.environ.get("PERSISTING_KEEP_TEST_ARTIFACTS") == "1"
        if keep or not success:
            print(f"Gateway {suite} fuzz artifacts: {work_dir}", file=sys.stderr)
        else:
            shutil.rmtree(work_dir)
    if suite in {"formats", "network-policy"}:
        detail = f"cases={call_count}"
    else:
        detail = f"calls={call_count} canonical_events={event_count}"
    print(f"PASS gateway {suite} fuzz: seed={seed} {detail}", flush=True)


if __name__ == "__main__":
    main()
