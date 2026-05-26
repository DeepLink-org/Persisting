#!/usr/bin/env python3
"""示例 Agent — 由 `persisting capture run … -- python3 agent.py` 执行。"""

from __future__ import annotations

import json
import os
import sys
import urllib.request

from dialogue_fixture import TURNS


def chat(base: str, user_message: str) -> str:
    payload = json.dumps(
        {"model": "mock-model", "messages": [{"role": "user", "content": user_message}]}
    ).encode("utf-8")
    req = urllib.request.Request(
        f"{base.rstrip('/')}/chat/completions",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.loads(resp.read())
    return data["choices"][0]["message"]["content"]


def main() -> int:
    base = os.environ.get("OPENAI_BASE_URL", "")
    session = os.environ.get("PERSISTING_CAPTURE_SESSION_ID", "")
    if not base:
        print(
            "请通过 capture run 运行，例如：\n"
            "  persisting capture run -o ./store -c proxy.toml -- python3 agent.py",
            file=sys.stderr,
        )
        return 2

    print(f"[agent] session={session}  OPENAI_BASE_URL={base}")

    for i, user_msg in enumerate(TURNS, start=1):
        reply = chat(base, user_msg)
        print(f"\n--- turn {i} ---")
        print(f"user: {user_msg}")
        print(f"assistant: {reply}")

    print("\n[agent] done → store/demo-agent/<session_id>/0001.md")
    return 0


if __name__ == "__main__":
    sys.exit(main())
