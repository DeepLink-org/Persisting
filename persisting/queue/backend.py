"""Persisting Queue Backend — Lance columnar storage engine.

Provides persistent storage backends:
- LanceBackend: Lance-based persistence with memory buffering and auto-flush
- PersistingBackend: LanceBackend + metrics collection

These are internal implementations. Users should use persisting.Queue instead:

    from persisting import Queue
    queue = Queue("my_topic", storage_path="./data")
    await queue.put({"id": "1", "value": 42})
"""

from __future__ import annotations

import asyncio
import logging
import shutil
import time
import pickle
from pathlib import Path
from typing import Any, AsyncIterator

from .metadata import BatchMeta
from .tensor_serde import ZEROCOPY_PREFIX, decode_rows, encode_rows

logger = logging.getLogger(__name__)

try:
    import lance
    import pyarrow as pa

    LANCE_AVAILABLE = True
except ImportError:
    LANCE_AVAILABLE = False
    logger.warning(
        "Lance not available. Install with: pip install persisting[lance] "
        "or pip install lance pyarrow"
    )


def _infer_arrow_table(records: list[dict[str, Any]]) -> "pa.Table":
    """Convert dicts to a PyArrow Table with basic type inference."""
    field_names: set[str] = set()
    for r in records:
        field_names.update(r.keys())

    arrays: dict[str, pa.Array] = {}
    for name in sorted(field_names):
        values = [r.get(name) for r in records]
        non_null = [v for v in values if v is not None]
        if non_null and all(isinstance(v, bool) for v in non_null):
            arrays[name] = pa.array(values, type=pa.bool_())
        elif non_null and all(isinstance(v, int) for v in non_null):
            arrays[name] = pa.array(values, type=pa.int64())
        elif non_null and all(isinstance(v, (int, float)) for v in non_null):
            arrays[name] = pa.array(values, type=pa.float64())
        elif non_null and all(isinstance(v, str) for v in non_null):
            arrays[name] = pa.array(values, type=pa.string())
        elif non_null and all(isinstance(v, (bytes, bytearray)) for v in non_null):
            arrays[name] = pa.array(
                [bytes(v) if v is not None else None for v in values],
                type=pa.binary(),
            )
        else:
            arrays[name] = pa.array(
                [pickle.dumps(v, protocol=pickle.HIGHEST_PROTOCOL) if v is not None else None for v in values],
                type=pa.binary(),
            )
    return pa.table(arrays)


def _table_to_dicts(table: "pa.Table") -> list[dict[str, Any]]:
    """Convert a PyArrow Table to a list of dicts."""
    columns = table.column_names
    rows: list[dict[str, Any]] = []
    for i in range(table.num_rows):
        row: dict[str, Any] = {}
        for col in columns:
            value = table[col][i].as_py()
            if isinstance(value, (bytes, bytearray)):
                raw = bytes(value)
                if raw.startswith(ZEROCOPY_PREFIX):
                    value = raw
                else:
                    try:
                        value = pickle.loads(raw)
                    except Exception:
                        value = raw
            row[col] = value
        rows.append(row)
    return rows


class LanceBackend:
    """Lance persistent storage backend for Pulsing streaming queues.

    Data flow: records → memory buffer → Lance dataset (on flush).
    Reads merge persisted data and in-memory buffer transparently.

    Auto-persist: Flush when buffer >= batch_size, and optionally every
    auto_flush_interval_sec (if > 0).

    Args:
        bucket_id: Bucket identifier.
        storage_path: Directory for Lance dataset files.
        batch_size: Auto-flush threshold (number of buffered records).
        auto_flush_interval_sec: If > 0, flush buffer to Lance at this interval (seconds).
    """

    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        auto_flush_interval_sec: float = 0.0,
        **kwargs: Any,
    ):
        self.bucket_id = bucket_id
        self.storage_path = Path(storage_path)
        self.batch_size = batch_size
        self.auto_flush_interval_sec = auto_flush_interval_sec
        self.zerocopy_mode = str(kwargs.get("zerocopy_mode", "auto"))

        self._buffer: list[dict[str, Any]] = []
        self._persisted_count: int = 0

        self._lock = asyncio.Lock()
        self._condition = asyncio.Condition(self._lock)
        self._flush_task: asyncio.Task[None] | None = None
        self._closed = False
        self._consumption_status: dict[str, set[int]] = {}
        self._zerocopy_stats: dict[str, int] = {
            "put_envelopes": 0,
            "read_envelopes": 0,
            "fallback_pickled": 0,
        }

        self.storage_path.mkdir(parents=True, exist_ok=True)
        self._dataset_path = self.storage_path / "data.lance"

        self._recover()

        if self.auto_flush_interval_sec > 0:
            self._flush_task = asyncio.create_task(self._auto_flush_loop())

    # ------------------------------------------------------------------
    # StorageBackend protocol
    # ------------------------------------------------------------------

    def total_count(self) -> int:
        return self._persisted_count + len(self._buffer)

    async def put(self, record: dict[str, Any]) -> None:
        async with self._condition:
            self._buffer.append(record)
            need_flush = len(self._buffer) >= self.batch_size
            self._condition.notify_all()

        if need_flush:
            await self.flush()
            async with self._condition:
                self._condition.notify_all()

    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        async with self._condition:
            self._buffer.extend(records)
            need_flush = len(self._buffer) >= self.batch_size
            self._condition.notify_all()

        if need_flush:
            await self.flush()
            async with self._condition:
                self._condition.notify_all()

    async def put_tensor(
        self,
        data: Any,
        *,
        partition_id: str = "default",
        custom_meta: dict[int, dict[str, Any]] | None = None,
    ) -> BatchMeta:
        """Put TensorDict-like data and return generated BatchMeta."""
        async with self._condition:
            start_index = self.total_count()
            rows, batch_meta = encode_rows(
                data,
                partition_id=partition_id,
                start_global_index=start_index,
                custom_meta=custom_meta,
                zerocopy_mode=self.zerocopy_mode,
            )
            self._buffer.extend(rows)
            for row in rows:
                for value in row.values():
                    if isinstance(value, (bytes, bytearray)):
                        raw = bytes(value)
                        if raw.startswith(ZEROCOPY_PREFIX):
                            self._zerocopy_stats["put_envelopes"] += 1
                        else:
                            self._zerocopy_stats["fallback_pickled"] += 1
            need_flush = len(self._buffer) >= self.batch_size
            self._condition.notify_all()

        if need_flush:
            await self.flush()
            async with self._condition:
                self._condition.notify_all()
        return batch_meta

    async def get(self, limit: int, offset: int) -> list[dict[str, Any]]:
        async with self._lock:
            return self._read(offset, limit)

    async def get_by_indices(self, indices: list[int]) -> list[dict[str, Any]]:
        """Read records by row indices (for sampler-aligned consumption).

        Indices are in logical order (0 = first record). Efficient for
        contiguous runs; non-contiguous indices are merged into runs internally.
        """
        if not indices:
            return []
        async with self._lock:
            out: list[dict[str, Any]] = []
            # Split into contiguous runs
            runs: list[tuple[int, int]] = []
            i = 0
            while i < len(indices):
                start = indices[i]
                j = i + 1
                while j < len(indices) and indices[j] == indices[j - 1] + 1:
                    j += 1
                runs.append((start, j - i))
                i = j
            for start, length in runs:
                out.extend(self._read(start, length))
            return out

    async def get_data(self, batch_meta: BatchMeta, fields: list[str] | None = None) -> Any:
        rows = await self.get_by_indices(batch_meta.global_indexes)
        for row in rows:
            for value in row.values():
                if isinstance(value, (bytes, bytearray)) and bytes(value).startswith(ZEROCOPY_PREFIX):
                    self._zerocopy_stats["read_envelopes"] += 1
        return decode_rows(rows, fields=fields or batch_meta.field_names)

    async def get_meta(
        self,
        fields: list[str],
        batch_size: int,
        task_name: str = "default",
        sampler: Any = None,
        **sampling_kwargs: Any,
    ) -> BatchMeta:
        from .metadata import FieldMeta, SampleMeta

        async with self._lock:
            total = self.total_count()
            consumed = self._consumption_status.setdefault(task_name, set())
            ready = [idx for idx in range(total) if idx not in consumed]

        if sampler is not None:
            sampled, marked = sampler.sample(ready, batch_size, **sampling_kwargs)
        else:
            sampled = ready[:batch_size]
            marked = sampled

        samples = [
            SampleMeta(
                partition_id=sampling_kwargs.get("partition_id", "default"),
                global_index=idx,
                fields={
                    field: FieldMeta(
                        name=field,
                        dtype=None,
                        shape=None,
                        production_status="ready",
                    )
                    for field in fields
                },
            )
            for idx in sampled
        ]

        if marked:
            async with self._lock:
                self._consumption_status.setdefault(task_name, set()).update(marked)
        return BatchMeta(samples=samples)

    async def mark_consumed(self, task_name: str, global_indexes: list[int]) -> None:
        async with self._lock:
            self._consumption_status.setdefault(task_name, set()).update(global_indexes)

    async def reset_consumption(self, task_name: str) -> None:
        async with self._lock:
            self._consumption_status.pop(task_name, None)

    async def clear(self, global_indexes: list[int]) -> None:
        if not global_indexes:
            return
        async with self._lock:
            all_rows = self._read(0, self.total_count())
            index_set = set(global_indexes)
            remained = [row for i, row in enumerate(all_rows) if i not in index_set]
            self._buffer = []
            self._persisted_count = len(remained)

            if LANCE_AVAILABLE:
                table = _infer_arrow_table(remained) if remained else None
                if table is None:
                    if self._dataset_path.exists():
                        shutil.rmtree(self._dataset_path, ignore_errors=True)
                else:
                    lance.write_dataset(table, self._dataset_path, mode="overwrite")
            else:
                self._buffer = remained

    async def get_stream(
        self,
        limit: int,
        offset: int,
        wait: bool = False,
        timeout: float | None = None,
    ) -> AsyncIterator[list[dict[str, Any]]]:
        pos = offset
        remaining = limit

        while remaining > 0:
            async with self._condition:
                if pos >= self.total_count():
                    if not wait:
                        return
                    try:
                        coro = self._condition.wait()
                        if timeout:
                            await asyncio.wait_for(coro, timeout=timeout)
                        else:
                            await coro
                        continue
                    except asyncio.TimeoutError:
                        return

                batch = self._read(pos, min(remaining, 100))

            if batch:
                yield batch
                pos += len(batch)
                remaining -= len(batch)
            elif not wait:
                break

    async def flush(self) -> None:
        async with self._lock:
            if not self._buffer:
                return
            to_write = self._buffer[:]
            self._buffer = []

        if not LANCE_AVAILABLE:
            logger.error("Lance not available — cannot persist data")
            async with self._lock:
                self._buffer = to_write + self._buffer
            return

        try:
            table = _infer_arrow_table(to_write)
            if self._dataset_path.exists():
                existing = lance.dataset(self._dataset_path).to_table()
                combined = pa.concat_tables([existing, table])
                lance.write_dataset(combined, self._dataset_path, mode="overwrite")
            else:
                lance.write_dataset(table, self._dataset_path, mode="create")

            self._persisted_count += len(to_write)
            logger.debug(f"LanceBackend[{self.bucket_id}] flushed {len(to_write)} records")
        except Exception as e:
            logger.error(f"LanceBackend[{self.bucket_id}] flush failed: {e}")
            async with self._lock:
                self._buffer = to_write + self._buffer

    async def stats(self) -> dict[str, Any]:
        async with self._lock:
            return {
                "bucket_id": self.bucket_id,
                "backend": "lance",
                "storage_path": str(self.storage_path),
                "zerocopy_mode": self.zerocopy_mode,
                "buffer_size": len(self._buffer),
                "persisted_count": self._persisted_count,
                "total_count": self.total_count(),
                "zerocopy": self._zerocopy_stats.copy(),
            }

    async def _auto_flush_loop(self) -> None:
        """Background task: flush buffer at auto_flush_interval_sec."""
        while not self._closed:
            try:
                await asyncio.sleep(self.auto_flush_interval_sec)
                if self._closed:
                    break
                await self.flush()
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.warning("LanceBackend[%s] auto-flush error: %s", self.bucket_id, e)

    def close(self) -> None:
        """Stop auto-flush task. Safe to call multiple times."""
        self._closed = True
        if self._flush_task and not self._flush_task.done():
            self._flush_task.cancel()

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _recover(self) -> None:
        """Recover persisted record count on startup."""
        if self._dataset_path.exists() and LANCE_AVAILABLE:
            try:
                self._persisted_count = lance.dataset(self._dataset_path).count_rows()
                logger.debug(
                    f"LanceBackend[{self.bucket_id}] recovered {self._persisted_count} records"
                )
            except Exception:
                pass

    def _read(self, offset: int, limit: int) -> list[dict[str, Any]]:
        """Read records: persisted first, then memory buffer."""
        records: list[dict[str, Any]] = []

        if offset < self._persisted_count and self._dataset_path.exists() and LANCE_AVAILABLE:
            try:
                ds = lance.dataset(self._dataset_path)
                n = min(limit, self._persisted_count - offset)
                table = ds.to_table(offset=offset, limit=n)
                if table.num_rows > 0:
                    records.extend(_table_to_dicts(table))
            except Exception as e:
                logger.error(f"LanceBackend[{self.bucket_id}] read error: {e}")

        if len(records) < limit:
            buf_offset = max(0, offset - self._persisted_count)
            buf_limit = limit - len(records)
            records.extend(self._buffer[buf_offset : buf_offset + buf_limit])

        return records


class PersistingBackend(LanceBackend):
    """LanceBackend with metrics collection.

    Adds lightweight counters on top of LanceBackend for observability.
    Future versions will add WAL and compaction support.

    Args:
        bucket_id: Bucket identifier.
        storage_path: Directory for Lance dataset files.
        batch_size: Auto-flush threshold.
        enable_metrics: Whether to collect operation metrics.
    """

    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        enable_metrics: bool = True,
        **kwargs: Any,
    ):
        super().__init__(
            bucket_id=bucket_id,
            storage_path=storage_path,
            batch_size=batch_size,
            **kwargs,
        )
        self.enable_metrics = enable_metrics
        self._metrics: dict[str, int | float] = {
            "put_count": 0,
            "get_count": 0,
            "flush_count": 0,
            "last_flush_time": 0.0,
        }

    async def put(self, record: dict[str, Any]) -> None:
        if self.enable_metrics:
            self._metrics["put_count"] += 1
        await super().put(record)

    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        if self.enable_metrics:
            self._metrics["put_count"] += len(records)
        await super().put_batch(records)

    async def get(self, limit: int, offset: int) -> list[dict[str, Any]]:
        if self.enable_metrics:
            self._metrics["get_count"] += 1
        return await super().get(limit, offset)

    async def flush(self) -> None:
        await super().flush()
        if self.enable_metrics:
            self._metrics["flush_count"] += 1
            self._metrics["last_flush_time"] = time.time()

    async def stats(self) -> dict[str, Any]:
        base = await super().stats()
        base["backend"] = "persisting"
        if self.enable_metrics:
            base["metrics"] = self._metrics.copy()
        return base

    def get_metrics(self) -> dict[str, int | float]:
        """Return a snapshot of collected metrics."""
        return self._metrics.copy()
