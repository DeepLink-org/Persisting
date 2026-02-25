"""
Persisting Queue 吞吐压测：可配置生产者/消费者数量，测试极限吞吐。

用法:
    # 从仓库根目录
    python benchmark/throughput_stress.py --producers 2 --consumers 2 --duration 10
    python benchmark/throughput_stress.py -p 4 -c 4 -d 30 --batch-size 50 --record-size 256

依赖: pip install persisting[lance]
"""

from __future__ import annotations

import argparse
import asyncio
import sys
import tempfile
import time
from pathlib import Path

if __name__ == "__main__" and __package__ is None:
    _root = Path(__file__).resolve().parent.parent
    sys.path.insert(0, str(_root))

from persisting import Queue

QUEUE_NAME = "bench_queue"


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Persisting Queue 吞吐压测")
    p.add_argument("-p", "--producers", type=int, default=2, help="生产者协程数 (默认 2)")
    p.add_argument("-c", "--consumers", type=int, default=2, help="消费者协程数 (默认 2)")
    p.add_argument("-d", "--duration", type=float, default=10.0, help="压测时长(秒) (默认 10)")
    p.add_argument("-b", "--batch-size", type=int, default=20, help="put/get 批大小 (默认 20)")
    p.add_argument("-r", "--record-size", type=int, default=0, help="每条记录 payload 字节数，0 表示最小 (默认 0)")
    p.add_argument("--storage-path", type=str, default="", help="持久化目录，默认用 /tmp 下临时目录")
    p.add_argument("--warmup", type=float, default=1.0, help="预热秒数 (默认 1)")
    return p.parse_args()


async def producer_worker(
    queue: Queue,
    worker_id: int,
    batch_size: int,
    record_size: int,
    end_time: float,
    total_produced: list[int],
) -> None:
    """单生产者协程：在 end_time 前尽可能多 put。"""
    local_count = 0
    payload = "x" * record_size if record_size else ""
    batch: list[dict] = []

    while time.monotonic() < end_time:
        for i in range(batch_size):
            batch.append({"id": f"w{worker_id}_{local_count + i}", "idx": local_count + i, "payload": payload})
        await queue.put_batch(batch)
        local_count += len(batch)
        batch.clear()
    total_produced[0] += local_count


async def consumer_worker(
    queue: Queue,
    end_time: float,
    drain_after: float,
    batch_size: int,
    shared_offset: list[int],
    offset_lock: asyncio.Lock,
    counts: list[int],
) -> None:
    """单消费者协程：在 end_time 前 get，之后 drain；用 shared_offset + lock 避免重复读。"""
    local_count = 0
    empty_streak = 0
    max_empty_streak = 50

    while True:
        now = time.monotonic()
        async with offset_lock:
            offset = shared_offset[0]
        batch = await queue.get(limit=batch_size, offset=offset)
        if not batch:
            if now >= drain_after:
                empty_streak += 1
                if empty_streak >= max_empty_streak:
                    break
            await asyncio.sleep(0.001)
            continue
        empty_streak = 0
        async with offset_lock:
            shared_offset[0] += len(batch)
        local_count += len(batch)
    counts[0] += local_count


async def run_benchmark(args: argparse.Namespace) -> tuple[int, int, float]:
    storage_path = args.storage_path or str(Path(tempfile.gettempdir()) / "persisting_bench" / "data")
    Path(storage_path).mkdir(parents=True, exist_ok=True)

    queue = Queue(
        QUEUE_NAME,
        storage_path=storage_path,
        batch_size=args.batch_size,
        num_buckets=4,
        bucket_column="id",
    )

    # 预热
    if args.warmup > 0:
        warmup_end = time.monotonic() + args.warmup
        for _ in range(max(1, int(args.warmup * 100))):
            await queue.put_batch([{"id": "w", "idx": 0}])
        await queue.flush()
        await queue.get(limit=100)
        while time.monotonic() < warmup_end:
            await asyncio.sleep(0.05)

    # 压测窗口
    start = time.monotonic()
    end_time = start + args.duration
    drain_after = end_time + 0.5  # 留一点时间给最后 flush，再开始 drain

    produced = [0]
    consumed = [0]
    shared_offset = [0]
    offset_lock = asyncio.Lock()

    producers = [
        asyncio.create_task(
            producer_worker(
                queue, i, args.batch_size, args.record_size, end_time, produced,
            )
        )
        for i in range(args.producers)
    ]
    consumers = [
        asyncio.create_task(
            consumer_worker(
                queue, end_time, drain_after, args.batch_size,
                shared_offset, offset_lock, consumed,
            )
        )
        for _ in range(args.consumers)
    ]

    await asyncio.gather(*producers)
    await queue.flush()
    await asyncio.gather(*consumers)

    elapsed = time.monotonic() - start
    queue.close()
    return produced[0], consumed[0], elapsed


def main() -> None:
    args = parse_args()
    if args.producers < 1 or args.consumers < 1:
        print("producers 和 consumers 至少为 1")
        sys.exit(1)
    if args.duration <= 0:
        print("duration 须大于 0")
        sys.exit(1)

    print("Persisting Queue 吞吐压测")
    print("  producers:", args.producers)
    print("  consumers:", args.consumers)
    print("  duration(s):", args.duration)
    print("  batch_size:", args.batch_size)
    print("  record_size(bytes):", args.record_size)
    print("  warmup(s):", args.warmup)
    print()

    produced, consumed, elapsed = asyncio.run(run_benchmark(args))

    print("结果:")
    print(f"  总生产: {produced} 条")
    print(f"  总消费: {consumed} 条")
    print(f"  实际耗时: {elapsed:.2f}s")
    print(f"  生产吞吐: {produced / args.duration:.0f} 条/s")
    print(f"  消费吞吐: {consumed / args.duration:.0f} 条/s")
    print(f"  综合吞吐(消费): {consumed / elapsed:.0f} 条/s")


if __name__ == "__main__":
    main()
