#!/usr/bin/env python3
"""Reference ATIF queries using only Python's standard JSON parser.

This is intentionally not a general SQL engine.  It defines the raw-file
baseline for the two queries used by the pChronicle performance examples.
Every invocation opens and parses the complete JSONL input.
"""

import json
import sys
from collections import Counter
from pathlib import Path


def documents(path: Path) -> list[dict]:
    with path.open(encoding="utf-8") as stream:
        return [json.loads(line) for line in stream if line.strip()]


def group_by_source(items: list[dict]) -> list[dict]:
    counts = Counter(
        step.get("source", "unknown") for document in items for step in document.get("steps", [])
    )
    return [{"source": source, "steps": counts[source]} for source in sorted(counts)]


def selective_steps(items: list[dict], session_id: str) -> list[dict]:
    rows = [
        {"step_id": step["step_id"], "source": step.get("source", "unknown")}
        for document in items
        if document.get("session_id") == session_id
        for step in document.get("steps", [])
        if 5 <= step.get("step_id", -1) <= 15
    ]
    return sorted(rows, key=lambda row: row["step_id"])


if len(sys.argv) not in (3, 4):
    raise SystemExit("usage: python_json_baseline.py INPUT {group|selective} [SESSION_ID]")

input_arg, query = sys.argv[1:3]
parsed = documents(Path(input_arg))
if query == "group" and len(sys.argv) == 3:
    result = group_by_source(parsed)
elif query == "selective" and len(sys.argv) == 4:
    result = selective_steps(parsed, sys.argv[3])
else:
    raise SystemExit("group takes no SESSION_ID; selective requires one SESSION_ID")

for row in result:
    print(json.dumps(row, separators=(",", ":"), ensure_ascii=False))
