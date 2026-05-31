#!/usr/bin/env python3
"""Simulated agent: multi-turn chat via OPENAI_BASE_URL (set by ``traj capture``)."""

from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request


def main() -> int:
    base = os.environ.get("OPENAI_BASE_URL", "").rstrip("/")
    if not base:
        print("OPENAI_BASE_URL not set — run under `persisting traj capture`", file=sys.stderr)
        return 2

    turns = int(os.environ.get("CAPTURE_AGENT_TURNS", "3"))
    manifest_path = os.environ.get("CAPTURE_AGENT_MANIFEST", "")
    if not manifest_path:
        print("CAPTURE_AGENT_MANIFEST not set", file=sys.stderr)
        return 2

    session = os.environ.get("PERSISTING_CAPTURE_SESSION_ID", "")
    print(f"[capture-run-agent] session={session} base={base} turns={turns}", flush=True)

    manifest: list[dict] = []
    for i in range(1, turns + 1):
        user_msg = f"turn-{i}"
        payload = {
            "model": "mock-model",
            "messages": [{"role": "user", "content": user_msg}],
        }
        req = urllib.request.Request(
            f"{base}/chat/completions",
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                data = json.loads(resp.read())
        except urllib.error.HTTPError as e:
            print(f"HTTP {e.code}: {e.read().decode()}", file=sys.stderr)
            return 1

        assistant = data["choices"][0]["message"]["content"]
        expected = f"mock-reply-to-{user_msg}"
        if assistant != expected:
            print(f"turn {i}: expected {expected!r}, got {assistant!r}", file=sys.stderr)
            return 1

        manifest.append(
            {
                "turn": i,
                "user": user_msg,
                "assistant": assistant,
                "session_id": session,
            }
        )
        print(f"[capture-run-agent] turn {i} ok: {user_msg} -> {assistant}", flush=True)

    with open(manifest_path, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")

    print(f"[capture-run-agent] wrote manifest ({len(manifest)} turns)", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
