#!/usr/bin/env python3
import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

from dialogue_fixture import REPLY_BY_USER


class Handler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        size = int(self.headers.get("Content-Length", "0"))
        request = json.loads(self.rfile.read(size))
        user_text = next(
            message["content"]
            for message in reversed(request["messages"])
            if message["role"] == "user"
        )
        content = REPLY_BY_USER[user_text]
        body = json.dumps(
            {
                "id": "mock-call",
                "model": "mock-model",
                "choices": [{"message": {"role": "assistant", "content": content}}],
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, message: str, *args: object) -> None:
        print(message % args, flush=True)


port = int(os.environ.get("MOCK_LLM_PORT", "19080"))
HTTPServer(("127.0.0.1", port), Handler).serve_forever()
