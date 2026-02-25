"""Persisting queue with TransferQueue-compatible Tensor/KV APIs."""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any, AsyncIterator

from .metadata import BatchMeta

if TYPE_CHECKING:
    from persisting.sampler import BaseSampler

logger = logging.getLogger(__name__)


def _pulsing_available() -> bool:
    try:
        import pulsing

        return pulsing.is_initialized()
    except (ImportError, Exception):
        return False


class Queue:
    """Queue with append mode and Tensor/KV-style metadata APIs."""

    def __init__(
        self,
        name: str,
        storage_path: str = "./data",
        *,
        batch_size: int = 100,
        auto_flush_interval_sec: float = 0.0,
        enable_metrics: bool = False,
        num_buckets: int = 4,
        bucket_column: str = "id",
        zerocopy_mode: str = "auto",
    ):
        self.name = name
        self._distributed = False
        self._writer = None
        self._reader = None
        self._backend = None
        self._zerocopy_mode = zerocopy_mode

        if _pulsing_available():
            self._init_distributed(name, storage_path, batch_size, num_buckets, bucket_column)
        else:
            self._init_local(
                name=name,
                storage_path=storage_path,
                batch_size=batch_size,
                auto_flush_interval_sec=auto_flush_interval_sec,
                enable_metrics=enable_metrics,
            )

    def _init_distributed(
        self,
        name: str,
        storage_path: str,
        batch_size: int,
        num_buckets: int,
        bucket_column: str,
    ) -> None:
        import pulsing
        from pulsing.streaming import register_backend

        from .backend import LanceBackend

        register_backend("lance", LanceBackend)
        self._distributed = True
        self._system = pulsing.get_system()
        self._topic = name
        self._storage_path = storage_path
        self._batch_size = batch_size
        self._num_buckets = num_buckets
        self._bucket_column = bucket_column
        logger.info("Queue '%s': using Pulsing distributed mode", name)

    def _init_local(
        self,
        *,
        name: str,
        storage_path: str,
        batch_size: int,
        auto_flush_interval_sec: float,
        enable_metrics: bool,
    ) -> None:
        path = f"{storage_path}/{name}"
        if enable_metrics:
            from .backend import PersistingBackend

            self._backend = PersistingBackend(
                bucket_id=0,
                storage_path=path,
                batch_size=batch_size,
                auto_flush_interval_sec=auto_flush_interval_sec,
                enable_metrics=True,
            )
        else:
            from .backend import LanceBackend

            self._backend = LanceBackend(
                bucket_id=0,
                storage_path=path,
                batch_size=batch_size,
                auto_flush_interval_sec=auto_flush_interval_sec,
            )
        logger.info("Queue '%s': using local mode (Pulsing not available)", name)

    async def _ensure_writer(self):
        if self._writer is None and self._distributed:
            from pulsing.streaming import write_queue

            self._writer = await write_queue(
                self._system,
                self._topic,
                bucket_column=self._bucket_column,
                num_buckets=self._num_buckets,
                batch_size=self._batch_size,
                storage_path=self._storage_path,
                backend="lance",
                backend_options={"zerocopy_mode": self._zerocopy_mode},
            )
        return self._writer

    async def _ensure_reader(self):
        if self._reader is None and self._distributed:
            from pulsing.streaming import read_queue

            self._reader = await read_queue(
                self._system,
                self._topic,
                num_buckets=self._num_buckets,
                storage_path=self._storage_path,
                backend="lance",
                backend_options={"zerocopy_mode": self._zerocopy_mode},
            )
        return self._reader

    async def _resolve_partition_bucket(self, partition_id: str):
        writer = await self._ensure_writer()
        queue = writer.queue
        bucket_id = queue.get_bucket_id(partition_id)
        return await queue._ensure_bucket(bucket_id)

    async def put(
        self,
        data: Any,
        partition_id: str = "default",
        fields: list[str] | None = None,
    ) -> BatchMeta | None:
        del fields
        if self._distributed:
            if isinstance(data, dict) or (isinstance(data, list) and data and isinstance(data[0], dict)):
                writer = await self._ensure_writer()
                await writer.put(data)
                return None
            bucket = await self._resolve_partition_bucket(partition_id)
            result = await bucket.put_tensor(data, partition_id=partition_id)
            return BatchMeta.from_dict(result) if isinstance(result, dict) and "samples" in result else None

        if isinstance(data, dict):
            await self._backend.put(data)
            return None
        if isinstance(data, list) and data and isinstance(data[0], dict):
            await self._backend.put_batch(data)
            return None
        return await self._backend.put_tensor(data, partition_id=partition_id)

    async def put_batch(
        self,
        records: list[dict[str, Any]],
        partition_id: str = "default",
    ) -> None:
        del partition_id
        if self._distributed:
            writer = await self._ensure_writer()
            await writer.put(records)
            return
        await self._backend.put_batch(records)

    async def flush(self) -> None:
        if self._distributed:
            writer = await self._ensure_writer()
            await writer.flush()
        else:
            await self._backend.flush()

    async def get(self, limit: int = 100, offset: int = 0) -> list[dict[str, Any]]:
        if self._distributed:
            reader = await self._ensure_reader()
            return await reader.get(limit=limit)
        return await self._backend.get(limit=limit, offset=offset)

    async def get_meta(
        self,
        fields: list[str],
        batch_size: int,
        task_name: str = "default",
        partition_id: str = "default",
        sampler: "BaseSampler | None" = None,
        **sampling_kwargs: Any,
    ) -> BatchMeta:
        if self._distributed:
            bucket = await self._resolve_partition_bucket(partition_id)
            raw = await bucket.get_meta(
                fields=fields,
                batch_size=batch_size,
                task_name=task_name,
                sampler=sampler,
                partition_id=partition_id,
                **sampling_kwargs,
            )
            return BatchMeta.from_dict(raw)

        return await self._backend.get_meta(
            fields=fields,
            batch_size=batch_size,
            task_name=task_name,
            sampler=sampler,
            partition_id=partition_id,
            **sampling_kwargs,
        )

    async def get_data(
        self,
        batch_meta: BatchMeta,
        partition_id: str = "default",
    ) -> Any:
        if self._distributed:
            bucket = await self._resolve_partition_bucket(partition_id)
            return await bucket.get_data(batch_meta.to_dict(), fields=batch_meta.field_names)
        return await self._backend.get_data(batch_meta, fields=batch_meta.field_names)

    async def get_batch(
        self,
        fields: list[str],
        batch_size: int,
        task_name: str = "default",
        partition_id: str = "default",
        sampler: "BaseSampler | None" = None,
        **sampling_kwargs: Any,
    ) -> Any:
        meta = await self.get_meta(
            fields=fields,
            batch_size=batch_size,
            task_name=task_name,
            partition_id=partition_id,
            sampler=sampler,
            **sampling_kwargs,
        )
        if meta.size == 0:
            return []
        return await self.get_data(meta, partition_id=partition_id)

    async def mark_consumed(self, task_name: str, global_indexes: list[int], partition_id: str = "default") -> None:
        if self._distributed:
            bucket = await self._resolve_partition_bucket(partition_id)
            await bucket.mark_consumed(task_name, global_indexes)
            return
        await self._backend.mark_consumed(task_name, global_indexes)

    async def reset_consumption(self, task_name: str, partition_id: str = "default") -> None:
        if self._distributed:
            bucket = await self._resolve_partition_bucket(partition_id)
            await bucket.reset_consumption(task_name)
            return
        await self._backend.reset_consumption(task_name)

    async def clear(self, global_indexes: list[int], partition_id: str = "default") -> None:
        if self._distributed:
            bucket = await self._resolve_partition_bucket(partition_id)
            await bucket.clear(global_indexes)
            return
        await self._backend.clear(global_indexes)

    async def stream(
        self,
        limit: int = 100,
        offset: int = 0,
        wait: bool = False,
        timeout: float | None = None,
    ) -> AsyncIterator[list[dict[str, Any]]]:
        if self._distributed:
            reader = await self._ensure_reader()
            records = await reader.get(limit=limit, wait=wait, timeout=timeout)
            if records:
                yield records
            return
        async for batch in self._backend.get_stream(limit=limit, offset=offset, wait=wait, timeout=timeout):
            yield batch

    async def stats(self) -> dict[str, Any]:
        if self._distributed:
            writer = await self._ensure_writer()
            return await writer.queue.stats()
        return await self._backend.stats()

    def writer(self) -> "QueueWriter":
        return QueueWriter(self)

    def reader(self) -> "QueueReader":
        return QueueReader(self)

    def __len__(self) -> int:
        if self._distributed:
            return 0
        return self._backend.total_count()

    def close(self) -> None:
        if not self._distributed and self._backend:
            self._backend.close()


class QueueWriter:
    def __init__(self, queue: Queue):
        self.queue = queue

    async def put(
        self,
        data: Any,
        partition_id: str = "default",
        fields: list[str] | None = None,
    ) -> BatchMeta | None:
        return await self.queue.put(data=data, partition_id=partition_id, fields=fields)

    async def flush(self) -> None:
        await self.queue.flush()

    async def put_batch(self, records: list[dict[str, Any]], partition_id: str = "default") -> None:
        await self.queue.put_batch(records=records, partition_id=partition_id)


class QueueReader:
    def __init__(self, queue: Queue):
        self.queue = queue

    async def get_meta(
        self,
        fields: list[str],
        batch_size: int,
        task_name: str = "default",
        partition_id: str = "default",
        sampler: "BaseSampler | None" = None,
        **sampling_kwargs: Any,
    ) -> BatchMeta:
        return await self.queue.get_meta(
            fields=fields,
            batch_size=batch_size,
            task_name=task_name,
            partition_id=partition_id,
            sampler=sampler,
            **sampling_kwargs,
        )

    async def get_data(self, batch_meta: BatchMeta, partition_id: str = "default") -> Any:
        return await self.queue.get_data(batch_meta=batch_meta, partition_id=partition_id)

    async def get_batch(
        self,
        fields: list[str],
        batch_size: int,
        task_name: str = "default",
        partition_id: str = "default",
        sampler: "BaseSampler | None" = None,
        **sampling_kwargs: Any,
    ) -> Any:
        return await self.queue.get_batch(
            fields=fields,
            batch_size=batch_size,
            task_name=task_name,
            partition_id=partition_id,
            sampler=sampler,
            **sampling_kwargs,
        )

    async def reset(self, task_name: str = "default", partition_id: str = "default") -> None:
        await self.queue.reset_consumption(task_name=task_name, partition_id=partition_id)
