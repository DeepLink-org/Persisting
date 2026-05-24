#!/usr/bin/env python3
"""校验 capture 生成的 0001.md（YAML frontmatter + 裸对话块）。"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

from dialogue_fixture import REPLIES, TURNS

BLOCK_RE = re.compile(rb"^<!-- persisting:block(?::\w+)? (\{.*\}) -->$", re.M)


def strip_frontmatter(raw: bytes) -> bytes:
    if not raw.startswith(b"---\n"):
        return raw
    end = raw.find(b"\n---\n", 4)
    return raw[end + 5 :] if end != -1 else raw


def skip_blank_lines(raw: bytes, pos: int) -> int:
    while pos < len(raw):
        nl = raw.find(b"\n", pos)
        if nl == -1:
            if raw[pos:].strip():
                break
            pos = len(raw)
            break
        if raw[pos:nl].strip():
            break
        pos = nl + 1
    return pos


def parse_blocks(raw: bytes) -> list[tuple[dict, str]]:
    body = strip_frontmatter(raw)
    blocks: list[tuple[dict, str]] = []
    for m in BLOCK_RE.finditer(body):
        header = json.loads(m.group(1))
        pos = m.end()
        if pos < len(body) and body[pos : pos + 1] == b"\n":
            pos += 1
        start = skip_blank_lines(body, pos)
        end = start + int(header["length"])
        blocks.append((header, body[start:end].decode("utf-8")))
    return blocks


def main() -> int:
    path = Path(sys.argv[1])
    raw = path.read_bytes()
    errors: list[str] = []

    if not raw.startswith(b"---\n"):
        errors.append("缺少 YAML frontmatter（应以 --- 开头）")
    elif b"persisting:1.0" not in raw[:512]:
        errors.append("frontmatter 应包含 format: persisting:1.0")

    blocks = parse_blocks(raw)

    if len(blocks) != 4:
        errors.append(f"块数应为 4，实际 {len(blocks)}")
    if [h.get("kind") for h, _ in blocks] != ["llm.request", "llm.response"] * 2:
        errors.append("kind 顺序应为 request/response × 2")

    for i, (h, text) in enumerate(blocks):
        if len(text.encode("utf-8")) != h.get("length"):
            errors.append(f"块 {i}: length 与正文字节数不一致")
        if text.startswith("{") and text.endswith("}"):
            errors.append(f"块 {i}: 正文不应为 JSON，应为裸对话 markdown")
        t = h.get("turn", 0) - 1
        if h.get("kind") == "llm.request" and t < len(TURNS) and text != TURNS[t]:
            errors.append(f"块 {i}: 用户消息不匹配")
        if h.get("kind") == "llm.response" and t < len(REPLIES) and text != REPLIES[t]:
            errors.append(f"块 {i}: 助手回复不匹配")

    if errors:
        print("check: FAIL", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1

    print("check: OK — frontmatter + 4 块裸对话正文，与 Mock 一致")
    return 0


if __name__ == "__main__":
    sys.exit(main())
