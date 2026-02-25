"""Streaming dataset based on Persisting QueueReader."""

from __future__ import annotations

import time
from typing import Any, Iterator

try:
    from torch.utils.data import IterableDataset
except Exception:
    class IterableDataset:  # type: ignore[no-redef]
        pass


class StreamingDataset(IterableDataset):
    """Simple IterableDataset pulling batches from QueueReader."""

    def __init__(
        self,
        reader: Any,
        data_fields: list[str],
        batch_size: int,
        task_name: str = "default",
        partition_id: str = "default",
        sampler: Any = None,
        empty_sleep_sec: float = 0.5,
        sampling_kwargs: dict[str, Any] | None = None,
    ):
        super().__init__()
        self.reader = reader
        self.data_fields = data_fields
        self.batch_size = batch_size
        self.task_name = task_name
        self.partition_id = partition_id
        self.sampler = sampler
        self.empty_sleep_sec = empty_sleep_sec
        self.sampling_kwargs = sampling_kwargs or {}

    def __iter__(self) -> Iterator[Any]:
        while True:
            data = self._next_batch()
            if not data:
                time.sleep(self.empty_sleep_sec)
                continue
            yield data

    def _next_batch(self) -> Any:
        import asyncio

        return asyncio.run(
            self.reader.get_batch(
                fields=self.data_fields,
                batch_size=self.batch_size,
                task_name=self.task_name,
                partition_id=self.partition_id,
                sampler=self.sampler,
                **self.sampling_kwargs,
            )
        )
