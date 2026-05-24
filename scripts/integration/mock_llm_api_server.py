#!/usr/bin/env python3
"""OpenAI-compatible mock LLM API for capture integration / stress / e2e tests."""

from __future__ import annotations

import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer


def resolve_port() -> int:
    for key in ("MOCK_LLM_PORT", "MOCK_UPSTREAM_PORT"):
        raw = os.environ.get(key, "").strip()
        if raw.isdigit() and int(raw) > 0:
            return int(raw)
    return 0


def log_request(path: str, body: bytes) -> None:
    log_path = os.environ.get("MOCK_REQUEST_LOG", "")
    if not log_path:
        return
    try:
        parsed = json.loads(body.decode("utf-8")) if body else {}
    except json.JSONDecodeError:
        parsed = {"raw": body.decode("utf-8", errors="replace")}
    with open(log_path, "a", encoding="utf-8") as f:
        f.write(json.dumps({"ts": time.time(), "path": path, "body": parsed}, ensure_ascii=False) + "\n")


def last_user_message(payload: dict) -> str | None:
    messages = payload.get("messages") or []
    for msg in reversed(messages):
        if msg.get("role") == "user":
            content = msg.get("content")
            if isinstance(content, str):
                return content
    return None


def assistant_content(payload: dict) -> str:
    user = last_user_message(payload)
    if user is None:
        return os.environ.get("MOCK_REPLY", "integration-ok")
    return f"mock-reply-to-{user}"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        if self.path.rstrip("/").endswith("/models"):
            body = json.dumps({"data": [{"id": "mock-model"}]}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_error(404)

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length else b""
        log_request(self.path, raw)

        if "chat/completions" not in self.path and not self.path.rstrip("/").endswith("/v1"):
            self.send_error(404)
            return

        try:
            payload = json.loads(raw.decode("utf-8")) if raw else {}
        except json.JSONDecodeError:
            payload = {}

        content = assistant_content(payload)
        body = json.dumps(
            {
                "id": "mock-chat-completion",
                "object": "chat.completion",
                "model": payload.get("model", "mock-model"),
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": content},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": 3,
                    "completion_tokens": max(1, len(content)),
                    "total_tokens": 3 + max(1, len(content)),
                },
            }
        ).encode()

        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args: object) -> None:
        sys.stderr.write(f"[mock-llm] {self.address_string()} - {fmt % args}\n")


def main() -> None:
    port = resolve_port()
    if port < 1:
        print("set MOCK_LLM_PORT or MOCK_UPSTREAM_PORT", file=sys.stderr)
        sys.exit(2)
    host = os.environ.get("MOCK_LLM_HOST", os.environ.get("MOCK_UPSTREAM_HOST", "127.0.0.1"))
    server = HTTPServer((host, port), Handler)
    print(f"mock LLM API on http://{host}:{port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
