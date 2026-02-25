"""
Persisting Example 04: Producer–Consumer

One producer writes to a queue; one consumer reads. Uses streaming read
for long-running consumption with backpressure.

Run:
    python -m examples.04_producer_consumer

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


async def run_producer_consumer(storage_path: str) -> None:
    """One producer, one consumer; consumer reads in batches."""
    print_header("Single producer, single consumer")

    queue = Queue("pc_queue", storage_path=storage_path)

    print("\n  Producer: writing 10 records...")
    for i in range(10):
        await queue.put({"id": f"p{i}", "idx": i})
    await queue.flush()

    print("  Consumer: reading in batches of 3...")
    total = 0
    offset = 0
    while True:
        batch = await queue.get(limit=3, offset=offset)
        if not batch:
            break
        total += len(batch)
        offset += len(batch)
        print(f"    got {len(batch)} records (total so far: {total})")
    print(f"  ✓ Consumer finished, total {total} records")

    queue.close()


async def run_streaming_consumer(storage_path: str) -> None:
    """Producer writes, consumer uses stream()."""
    print_header("Streaming consumer")

    queue = Queue("pc_stream", storage_path=storage_path)

    print("\n  Producer: writing 6 records...")
    for i in range(6):
        await queue.put({"id": f"s{i}", "idx": i})
    await queue.flush()

    print("  Consumer: streaming read...")
    total = 0
    async for batch in queue.stream(limit=100):
        total += len(batch)
        print(f"    stream batch: {len(batch)} records (total: {total})")
    print(f"  ✓ Streaming consumer finished, total {total} records")

    queue.close()


async def main() -> None:
    storage_base = Path(__file__).resolve().parent / "data" / "example04"
    storage_base.mkdir(parents=True, exist_ok=True)

    await run_producer_consumer(str(storage_base / "single"))
    await run_streaming_consumer(str(storage_base / "stream"))

    print("\n" + "=" * 70)
    print("Example 04 complete.")
    print("=" * 70)


if __name__ == "__main__":
    asyncio.run(main())
