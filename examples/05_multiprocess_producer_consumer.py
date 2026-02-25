"""
Persisting Example 05: 多进程同时写入和消费，持久化到 /tmp

- 多进程：1 个生产者进程 + 1 个消费者进程，共享同一持久化目录。
- 持久化：Lance 落盘到 /tmp 下指定目录，生产者 flush 后消费者即可读到。

Run (from repo root):
    python -m examples.05_multiprocess_producer_consumer

Requires: pip install persisting[lance]
"""

from __future__ import annotations

import asyncio
import multiprocessing
import sys
import tempfile
from pathlib import Path

if __name__ == "__main__" and __package__ is None:
    _root = Path(__file__).resolve().parent.parent
    sys.path.insert(0, str(_root))

from persisting import Queue

# 总条数、批大小，供 producer/consumer 共用
TOTAL_RECORDS = 50
BATCH_SIZE = 10
QUEUE_NAME = "mp_queue"


async def _producer_async(storage_path: str) -> None:
    """生产者：往队列写记录并定期 flush，持久化到 storage_path。"""
    queue = Queue(QUEUE_NAME, storage_path=storage_path, batch_size=BATCH_SIZE)
    for i in range(TOTAL_RECORDS):
        await queue.put({"id": f"rec_{i}", "idx": i, "payload": f"data_{i}"})
        if (i + 1) % 5 == 0:
            await queue.flush()
            print(f"  [Producer] flushed {i + 1} records")
    await queue.flush()
    print(f"  [Producer] done, total {TOTAL_RECORDS} records, storage: {storage_path}")
    queue.close()


async def _consumer_async(storage_path: str) -> None:
    """消费者：从同一 storage_path 读队列，直到读满 TOTAL_RECORDS 或短时无数据。"""
    queue = Queue(QUEUE_NAME, storage_path=storage_path, batch_size=BATCH_SIZE)
    total_read = 0
    offset = 0
    empty_retries = 0
    max_empty_retries = 20

    while total_read < TOTAL_RECORDS and empty_retries < max_empty_retries:
        batch = await queue.get(limit=BATCH_SIZE, offset=offset)
        if not batch:
            empty_retries += 1
            await asyncio.sleep(0.2)
            continue
        empty_retries = 0
        total_read += len(batch)
        offset += len(batch)
        print(f"  [Consumer] got {len(batch)} records (total so far: {total_read})")
    print(f"  [Consumer] done, total read {total_read}, storage: {storage_path}")
    queue.close()


def _run_producer(storage_path: str) -> None:
    asyncio.run(_producer_async(storage_path))


def _run_consumer(storage_path: str) -> None:
    asyncio.run(_consumer_async(storage_path))


def main() -> None:
    # 持久化到 /tmp 下临时目录
    base = Path(tempfile.gettempdir()) / "persisting_mp_example"
    base.mkdir(parents=True, exist_ok=True)
    storage_path = str(base / "data")
    print("=" * 60)
    print("Multi-process producer–consumer, persisted under /tmp")
    print("=" * 60)
    print(f"  storage_path: {storage_path}")
    print(f"  total records: {TOTAL_RECORDS}, batch: {BATCH_SIZE}\n")

    producer = multiprocessing.Process(target=_run_producer, args=(storage_path,))
    consumer = multiprocessing.Process(target=_run_consumer, args=(storage_path,))

    producer.start()
    consumer.start()
    producer.join()
    consumer.join()

    print("\n" + "=" * 60)
    print("Example 05 complete. Data remains under:")
    print(f"  {storage_path}")
    print("=" * 60)


if __name__ == "__main__":
    main()
