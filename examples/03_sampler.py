"""
Persisting Example 03: Sampler (TQ-aligned)

Shows SequentialSampler and queue.get_batch(): consume in sampler order with
consumed_offset. Also implements a simple custom sampler (reverse order).

Run:
    python -m examples.03_sampler

Requires: pip install persisting[lance]
"""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path
from typing import Any

if __name__ == "__main__" and __package__ is None:
    _root = Path(__file__).resolve().parent.parent
    sys.path.insert(0, str(_root))

from persisting import Queue, SequentialSampler
from persisting.sampler import BaseSampler


def print_header(title: str) -> None:
    print("=" * 70)
    print(title)
    print("=" * 70)


class ReverseSampler(BaseSampler):
    """Consume from the end of ready_indexes (reverse order)."""

    def sample(
        self,
        ready_indexes: list[int],
        batch_size: int,
        *args: Any,
        **kwargs: Any,
    ) -> tuple[list[int], list[int]]:
        taken = ready_indexes[-batch_size:] if len(ready_indexes) >= batch_size else ready_indexes[:]
        taken = list(reversed(taken))
        return taken, taken


async def demo_sequential_sampler() -> None:
    """SequentialSampler: consume in order via queue.get_batch()."""
    print_header("SequentialSampler + queue.get_batch()")

    storage_path = str(Path(__file__).resolve().parent / "data" / "example03_seq")
    queue = Queue("sampler_seq", storage_path=storage_path)

    for i in range(5):
        await queue.put({"id": str(i), "value": i * 100})
    await queue.flush()
    print(f"\n  Queue length = {len(queue)}")

    sampler = SequentialSampler()
    offset = 0
    batch_size = 2

    print(f"\n  Reading in batches of {batch_size} with SequentialSampler:")
    for step in range(3):
        records, offset = await queue.get_batch(sampler, batch_size=batch_size, consumed_offset=offset)
        if not records:
            break
        print(f"    Step {step + 1}: got {len(records)} records, new offset = {offset}")
        for r in records:
            print(f"      id={r.get('id')} value={r.get('value')}")

    queue.close()


async def demo_custom_reverse_sampler() -> None:
    """Custom ReverseSampler: consume in reverse order."""
    print_header("Custom Sampler (ReverseSampler)")

    storage_path = str(Path(__file__).resolve().parent / "data" / "example03_rev")
    queue = Queue("sampler_rev", storage_path=storage_path)

    for i in range(5):
        await queue.put({"id": str(i), "value": i * 10})
    await queue.flush()

    sampler = ReverseSampler()
    offset = 0
    batch_size = 2

    print("\n  Reading in batches of 2 with ReverseSampler (last indices first):")
    for step in range(3):
        records, offset = await queue.get_batch(sampler, batch_size=batch_size, consumed_offset=offset)
        if not records:
            break
        print(f"    Step {step + 1}: got {len(records)} records")
        for r in records:
            print(f"      id={r.get('id')} value={r.get('value')}")

    queue.close()


async def main() -> None:
    await demo_sequential_sampler()
    await demo_custom_reverse_sampler()

    print("\n" + "=" * 70)
    print("Example 03 complete. Custom samplers: subclass BaseSampler, implement sample().")
    print("=" * 70)


if __name__ == "__main__":
    asyncio.run(main())
