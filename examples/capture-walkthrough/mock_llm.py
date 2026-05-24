#!/usr/bin/env python3
"""本地 Mock LLM（OpenAI + Anthropic）。由 run.sh 启动，无需 API Key。"""

from __future__ import annotations

import json
import os
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

from dialogue_fixture import REPLY_BY_USER

HOST = os.environ.get("MOCK_LLM_HOST", "127.0.0.1")
PORT = int(os.environ.get("MOCK_LLM_PORT", "19080"))


def extract_user_text(payload: dict) -> str:
    for msg in reversed(payload.get("messages") or []):
        if msg.get("role") != "user":
            continue
        content = msg.get("content")
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            parts = [
                b.get("text", "")
                for b in content
                if isinstance(b, dict) and b.get("type") == "text" and b.get("text")
            ]
            if parts:
                return parts[-1]
    return ""


def openai_reply(payload: dict, user: str) -> bytes:
    content = REPLY_BY_USER.get(user, f"（mock）收到：{user}")
    return json.dumps(
        {
            "id": "walkthrough-mock",
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
                "prompt_tokens": max(1, len(user)),
                "completion_tokens": len(content),
                "total_tokens": max(1, len(user)) + len(content),
            },
        },
        ensure_ascii=False,
    ).encode()


def anthropic_reply(payload: dict, user: str) -> bytes:
    content = REPLY_BY_USER.get(user, f"（mock）你好！收到：{user}")
    return json.dumps(
        {
            "id": "msg_walkthrough_mock",
            "type": "message",
            "role": "assistant",
            "model": payload.get("model", "mock-model"),
            "content": [{"type": "text", "text": content}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": max(1, len(user)),
                "output_tokens": len(content),
            },
        },
        ensure_ascii=False,
    ).encode()


class Handler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        n = int(self.headers.get("Content-Length", "0"))
        payload = json.loads(self.rfile.read(n).decode() if n else "{}")
        user = extract_user_text(payload)

        if "chat/completions" in self.path:
            body = openai_reply(payload, user)
        elif "/messages" in self.path:
            body = anthropic_reply(payload, user)
        else:
            self.send_error(404, f"unsupported path: {self.path}")
            return

        self.send_response(200)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args: object) -> None:
        sys.stderr.write(f"[mock-llm] {fmt % args}\n")


if __name__ == "__main__":
    print(
        f"Mock LLM: http://{HOST}:{PORT}"
        f"  (/v1/chat/completions, /v1/messages)  Ctrl+C 停止"
    )
    HTTPServer((HOST, PORT), Handler).serve_forever()
