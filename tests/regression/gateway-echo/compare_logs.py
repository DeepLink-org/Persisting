#!/usr/bin/env python3
"""Join Python SDK results with canonical Gateway events and compare them."""

from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path
from typing import Any


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]


def parts_text(parts: list[dict[str, Any]]) -> str:
    return "".join(part.get("text", "") for part in parts if part.get("type") == "text")


def canonical_request_text(payload: dict[str, Any]) -> str:
    messages = payload["llm_request"]["request"]["messages"]
    return "".join(
        parts_text(message.get("parts", []))
        for message in messages
        if message.get("role") == "user"
    )


def canonical_response_text(payload: dict[str, Any]) -> str:
    candidates = payload["llm_response"]["response"].get("candidates", [])
    return "".join(
        parts_text(candidate.get("message", {}).get("parts", [])) for candidate in candidates
    )


def compare_logs(clients_path: Path, events_path: Path, output_path: Path) -> tuple[int, int]:
    clients = read_jsonl(clients_path)
    events = read_jsonl(events_path)
    by_session: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for event in events:
        event["record"] = json.loads(event["payload_json"])
        by_session[event["session_id"]].append(event)

    expected_sessions = {entry["session_id"] for entry in clients}
    checks: list[dict[str, Any]] = []
    for entry in clients:
        session_id = entry["session_id"]
        contract = entry["expected_capture"]
        session_events = by_session.get(session_id, [])
        by_kind = {event["kind"]: event for event in session_events}
        request = by_kind.get("llm.request")
        response = by_kind.get(contract["response_kind"])

        assertions = {
            "two_events": len(session_events) == 2,
            "request_present": request is not None,
            "response_present": response is not None,
        }
        if request is not None and response is not None:
            request_payload = request["record"]["payload"]
            response_payload = response["record"]["payload"]
            captured_input = canonical_request_text(request_payload)
            captured_output = canonical_response_text(response_payload)
            assertions.update(
                {
                    "call_id": bool(request["call_id"])
                    and request["call_id"] == response["call_id"],
                    "model": request["model"] == entry["model"],
                    "protocol": request_payload["protocol"] == entry["protocol"],
                    "provider": request_payload["provider"] == contract["provider"],
                    "input": captured_input == entry["input"],
                    "output": captured_output == entry["output"],
                    "status": response_payload["status"] == 200,
                    "upstream_path": response_payload["http"]["url"].endswith(
                        contract["upstream_path"]
                    ),
                    "forward_to": (
                        request_payload.get("forward_to") == contract["forward_to"]
                        and response_payload.get("forward_to") == contract["forward_to"]
                    ),
                }
            )

        checks.append(
            {
                "session_id": session_id,
                "client": entry["client"],
                "sdk_version": entry["sdk_version"],
                "protocol": entry["protocol"],
                "client_input": entry["input"],
                "captured_input": (
                    canonical_request_text(request["record"]["payload"]) if request else None
                ),
                "client_output": entry["output"],
                "captured_output": (
                    canonical_response_text(response["record"]["payload"]) if response else None
                ),
                "assertions": assertions,
                "matched": all(assertions.values()),
            }
        )

    unexpected_sessions = set(by_session) - expected_sessions
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", encoding="utf-8") as handle:
        for check in checks:
            json.dump(check, handle, ensure_ascii=False, sort_keys=True)
            handle.write("\n")

    failures = [check for check in checks if not check["matched"]]
    assert len(clients) == 4, f"expected 4 SDK calls, got {len(clients)}"
    assert len(events) == 8, f"expected 8 canonical events, got {len(events)}"
    assert not unexpected_sessions, f"unexpected captured sessions: {sorted(unexpected_sessions)}"
    assert not failures, json.dumps(failures, ensure_ascii=False, indent=2)
    return len(clients), len(events)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--clients", type=Path, required=True)
    parser.add_argument("--events", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    client_count, event_count = compare_logs(args.clients, args.events, args.output)
    print(f"compared {client_count} SDK calls with {event_count} canonical events")


if __name__ == "__main__":
    main()
