"""
Persisting Example 02: Queue Options

Shows Queue options: batch_size, auto_flush_interval_sec, enable_metrics.

Run:
    python -m examples.02_queue_options

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


async def demo_batch_size_and_stats() -> None:
    """batch_size controls auto-flush threshold; stats() shows queue state."""
    print_header("Queue: batch_size & stats")

    storage_path = str(Path(__file__).resolve().parent / "data" / "example02" / "options")
    queue = Queue("demo_options", storage_path=storage_path, batch_size=5)

    print("\n  Putting 3 records (buffer < batch_size=5, no auto-flush)...")
    for i in range(3):
        await queue.put({"id": str(i), "x": i * 10})
    stats_before = await queue.stats()
    print(f"  Stats: {stats_before}")

    print("\n  Flush explicitly...")
    await queue.flush()
    stats_after = await queue.stats()
    print(f"  Stats after flush: {stats_after}")

    got = await queue.get(limit=10)
    print(f"  Read back {len(got)} records.")
    queue.close()


async def demo_metrics() -> None:
    """enable_metrics=True adds operation counters."""
    print_header("Queue: enable_metrics")

    storage_path = str(Path(__file__).resolve().parent / "data" / "example02" / "metrics")
    queue = Queue("demo_metrics", storage_path=storage_path, enable_metrics=True)

    for i in range(4):
        await queue.put({"id": f"m{i}", "v": i})
    await queue.flush()
    stats = await queue.stats()
    print(f"\n  Stats (with metrics): {stats}")
    queue.close()


async def main() -> None:
    await demo_batch_size_and_stats()
    await demo_metrics()

    print("\n" + "=" * 70)
    print("Example 02 complete.")
    print("=" * 70)


if __name__ == "__main__":
    asyncio.run(main())
