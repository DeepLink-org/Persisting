"""
Persisting Example 01: Core Components & Data Workflow

Core workflow: Queue("name") → put → flush → get.
Data is persisted to Lance on flush. If Pulsing is running, queue is distributed.

Run (from repo root):
    python -m examples.01_core_components

Requires: pip install persisting[lance]
"""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path

if __name__ == "__main__" and __package__ is None:
    _root = Path(__file__).resolve().parent.parent
    sys.path.insert(0, str(_root))

from persisting import Queue


def print_header(title: str) -> None:
    print("=" * 70)
    print(title)
    print("=" * 70)


def print_step(step: str) -> None:
    print(f"\n[{step}]")


async def demonstrate_data_workflow(storage_path: str) -> None:
    """Put → flush → read with Persisting Queue."""
    print_header("Data Workflow: put → flush → get")

    print_step("Step 1 – Create queue")
    queue = Queue("tutorial_queue", storage_path=storage_path)
    print("  ✓ Queue created")

    print_step("Step 2 – Put records")
    records = [
        {"id": "1", "value": 100, "name": "alice"},
        {"id": "2", "value": 200, "name": "bob"},
        {"id": "3", "value": 300, "name": "carol"},
    ]
    for r in records:
        await queue.put(r)
    print(f"  ✓ Written {len(records)} records (buffered in memory)")

    print_step("Step 3 – Flush to disk")
    await queue.flush()
    print("  ✓ Flushed: data persisted to Lance dataset")

    print_step("Step 4 – Read back")
    got = await queue.get(limit=10)
    print(f"  ✓ Read {len(got)} records")
    for i, r in enumerate(got):
        print(f"    [{i}] id={r.get('id')} value={r.get('value')} name={r.get('name')}")

    print_step("Step 5 – Verify")
    assert len(got) == len(records)
    by_id = {r["id"]: r for r in got}
    for r in records:
        assert r["id"] in by_id and by_id[r["id"]]["value"] == r["value"]
    print("  ✓ Data matches original")

    queue.close()


async def main() -> None:
    print("""
    Persisting Example 01: Core Components

    Queue("name", storage_path="./data")
      → put(record)   — buffer in memory
      → flush()        — persist to Lance
      → get(limit=N)   — read back (persisted + buffer)
    """)
    print("=" * 70)

    storage_path = str(Path(__file__).resolve().parent / "data" / "example01")

    await demonstrate_data_workflow(storage_path)

    print("\n" + "=" * 70)
    print("Example 01 complete.")
    print("=" * 70)


if __name__ == "__main__":
    asyncio.run(main())
