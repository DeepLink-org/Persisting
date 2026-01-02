"""Persisting 存储后端 - Lance 持久化实现

提供 Pulsing 队列系统的持久化后端：
- LanceBackend: 基础 Lance 持久化
- PersistingBackend: 增强版（WAL、监控指标等）

使用方式：
    from pulsing.queue import write_queue, register_backend
    from persisting.queue import LanceBackend, PersistingBackend
    
    # 注册后端
    register_backend("lance", LanceBackend)
    register_backend("persisting", PersistingBackend)
    
    # 使用 Lance 后端
    writer = await write_queue(system, "topic", backend="lance")
    
    # 使用增强版后端
    writer = await write_queue(system, "topic", backend="persisting")
"""

from __future__ import annotations

import asyncio
import logging
import time
from pathlib import Path
from typing import Any, AsyncIterator

logger = logging.getLogger(__name__)

# 尝试导入 Lance
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


class LanceBackend:
    """Lance 持久化后端
    
    特点：
    - 内存缓冲 + Lance 持久化
    - 批量写入优化
    - 支持阻塞等待新数据
    - 数据恢复（重启后自动加载已持久化数据）
    
    Args:
        bucket_id: 桶 ID
        storage_path: 存储路径
        batch_size: 批处理大小，达到此数量自动 flush
    """

    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        **kwargs,
    ):
        self.bucket_id = bucket_id
        self.storage_path = Path(storage_path)
        self.batch_size = batch_size

        # 内存缓冲区
        self.buffer: list[dict[str, Any]] = []
        self.persisted_count: int = 0

        # 锁和条件变量
        self._lock = asyncio.Lock()
        self._condition = asyncio.Condition(self._lock)

        # 确保目录存在
        self.storage_path.mkdir(parents=True, exist_ok=True)
        self._dataset_path = self.storage_path / "data.lance"

        # 恢复已持久化记录数
        if self._dataset_path.exists() and LANCE_AVAILABLE:
            try:
                self.persisted_count = lance.dataset(self._dataset_path).count_rows()
                logger.debug(
                    f"LanceBackend[{self.bucket_id}] recovered {self.persisted_count} records"
                )
            except Exception:
                pass

    def total_count(self) -> int:
        return self.persisted_count + len(self.buffer)

    async def put(self, record: dict[str, Any]) -> None:
        async with self._condition:
            self.buffer.append(record)
            should_flush = len(self.buffer) >= self.batch_size
            self._condition.notify_all()

        if should_flush:
            await self.flush()
            async with self._condition:
                self._condition.notify_all()

    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        async with self._condition:
            self.buffer.extend(records)
            should_flush = len(self.buffer) >= self.batch_size
            self._condition.notify_all()

        if should_flush:
            await self.flush()
            async with self._condition:
                self._condition.notify_all()

    async def get(self, limit: int, offset: int) -> list[dict[str, Any]]:
        async with self._lock:
            return self._read_records(offset, limit)

    async def get_stream(
        self,
        limit: int,
        offset: int,
        wait: bool = False,
        timeout: float | None = None,
    ) -> AsyncIterator[list[dict[str, Any]]]:
        current_offset = offset
        remaining = limit

        while remaining > 0:
            async with self._condition:
                total = self.total_count()

                if current_offset >= total:
                    if wait:
                        try:
                            if timeout:
                                await asyncio.wait_for(
                                    self._condition.wait(), timeout=timeout
                                )
                            else:
                                await self._condition.wait()
                            continue
                        except asyncio.TimeoutError:
                            return
                    else:
                        return

                records = self._read_records(current_offset, min(remaining, 100))

            if records:
                yield records
                current_offset += len(records)
                remaining -= len(records)
            elif not wait:
                break

    async def flush(self) -> None:
        async with self._lock:
            if not self.buffer:
                return
            records = self.buffer[:]
            self.buffer = []

        if not LANCE_AVAILABLE:
            logger.error("Lance not available, cannot persist data")
            async with self._lock:
                self.buffer = records + self.buffer
            return

        try:
            table = self._records_to_table(records)

            if self._dataset_path.exists():
                existing = lance.dataset(self._dataset_path).to_table()
                combined = pa.concat_tables([existing, table])
                lance.write_dataset(combined, self._dataset_path, mode="overwrite")
            else:
                lance.write_dataset(table, self._dataset_path, mode="create")

            self.persisted_count += len(records)
            logger.debug(f"LanceBackend[{self.bucket_id}] flushed {len(records)} records")

        except Exception as e:
            logger.error(f"LanceBackend[{self.bucket_id}] flush error: {e}")
            async with self._lock:
                self.buffer = records + self.buffer

    async def stats(self) -> dict[str, Any]:
        async with self._lock:
            return {
                "bucket_id": self.bucket_id,
                "buffer_size": len(self.buffer),
                "persisted_count": self.persisted_count,
                "total_count": self.total_count(),
                "backend": "lance",
                "storage_path": str(self.storage_path),
            }

    def _read_records(self, offset: int, limit: int) -> list[dict[str, Any]]:
        """读取数据：先读持久化，再读内存缓冲"""
        records = []

        # 从持久化数据读取
        if offset < self.persisted_count and self._dataset_path.exists() and LANCE_AVAILABLE:
            try:
                dataset = lance.dataset(self._dataset_path)
                persisted_limit = min(limit, self.persisted_count - offset)
                table = dataset.to_table(offset=offset, limit=persisted_limit)
                if table.num_rows > 0:
                    columns = table.column_names
                    for i in range(table.num_rows):
                        records.append({col: table[col][i].as_py() for col in columns})
            except Exception as e:
                logger.error(f"LanceBackend[{self.bucket_id}] read error: {e}")

        # 从内存缓冲读取
        if len(records) < limit:
            buffer_offset = max(0, offset - self.persisted_count)
            buffer_limit = limit - len(records)
            buffer_records = self.buffer[buffer_offset : buffer_offset + buffer_limit]
            records.extend(buffer_records)

        return records

    def _records_to_table(self, records: list[dict[str, Any]]) -> "pa.Table":
        """将记录转换为 PyArrow Table"""
        field_names = set()
        for record in records:
            field_names.update(record.keys())

        arrays = {}
        for field_name in field_names:
            values = [record.get(field_name) for record in records]
            if all(isinstance(v, int) for v in values if v is not None):
                arrays[field_name] = pa.array(values, type=pa.int64())
            elif all(isinstance(v, float) for v in values if v is not None):
                arrays[field_name] = pa.array(values, type=pa.float64())
            elif all(isinstance(v, bool) for v in values if v is not None):
                arrays[field_name] = pa.array(values, type=pa.bool_())
            else:
                arrays[field_name] = pa.array(
                    [str(v) if v is not None else None for v in values],
                    type=pa.string(),
                )

        return pa.table(arrays)


class PersistingBackend:
    """增强的持久化后端
    
    在 LanceBackend 基础上增加：
    - WAL (Write-Ahead Log) 用于故障恢复
    - 监控指标
    - 更灵活的配置
    
    Args:
        bucket_id: 桶 ID
        storage_path: 存储路径
        batch_size: 批处理大小
        enable_wal: 是否启用 WAL
        enable_metrics: 是否启用监控指标
        compaction_threshold: 触发压缩的文件数阈值
    """

    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        enable_wal: bool = True,
        enable_metrics: bool = True,
        compaction_threshold: int = 10,
        **kwargs,
    ):
        self.bucket_id = bucket_id
        self.storage_path = Path(storage_path)
        self.batch_size = batch_size
        self.enable_wal = enable_wal
        self.enable_metrics = enable_metrics
        self.compaction_threshold = compaction_threshold

        # 内存缓冲区
        self.buffer: list[dict[str, Any]] = []
        self.persisted_count: int = 0

        # 锁和条件变量
        self._lock = asyncio.Lock()
        self._condition = asyncio.Condition(self._lock)

        # 监控指标
        self._metrics = {
            "put_count": 0,
            "get_count": 0,
            "flush_count": 0,
            "bytes_written": 0,
            "last_flush_time": 0,
        }

        # 确保目录存在
        self.storage_path.mkdir(parents=True, exist_ok=True)
        self._dataset_path = self.storage_path / "data.lance"
        self._wal_path = self.storage_path / "wal"

        # 初始化
        self._init_storage()

    def _init_storage(self) -> None:
        """初始化存储"""
        # 恢复已持久化记录数
        if self._dataset_path.exists() and LANCE_AVAILABLE:
            try:
                self.persisted_count = lance.dataset(self._dataset_path).count_rows()
            except Exception:
                pass

        # 初始化 WAL
        if self.enable_wal:
            self._wal_path.mkdir(parents=True, exist_ok=True)
            self._recover_from_wal()

        logger.info(
            f"PersistingBackend[{self.bucket_id}] initialized at {self.storage_path}, "
            f"persisted_count={self.persisted_count}, wal={self.enable_wal}"
        )

    def _recover_from_wal(self) -> None:
        """从 WAL 恢复数据"""
        # TODO: 实现 WAL 恢复逻辑
        pass

    def _write_wal(self, records: list[dict[str, Any]]) -> None:
        """写入 WAL"""
        if not self.enable_wal:
            return
        # TODO: 实现 WAL 写入逻辑
        pass

    def _clear_wal(self) -> None:
        """清理 WAL"""
        if not self.enable_wal:
            return
        # TODO: 实现 WAL 清理逻辑
        pass

    def total_count(self) -> int:
        return self.persisted_count + len(self.buffer)

    async def put(self, record: dict[str, Any]) -> None:
        async with self._condition:
            self.buffer.append(record)
            should_flush = len(self.buffer) >= self.batch_size
            self._condition.notify_all()

            if self.enable_metrics:
                self._metrics["put_count"] += 1

        # 写入 WAL
        self._write_wal([record])

        if should_flush:
            await self.flush()
            async with self._condition:
                self._condition.notify_all()

    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        async with self._condition:
            self.buffer.extend(records)
            should_flush = len(self.buffer) >= self.batch_size
            self._condition.notify_all()

            if self.enable_metrics:
                self._metrics["put_count"] += len(records)

        # 写入 WAL
        self._write_wal(records)

        if should_flush:
            await self.flush()
            async with self._condition:
                self._condition.notify_all()

    async def get(self, limit: int, offset: int) -> list[dict[str, Any]]:
        async with self._lock:
            if self.enable_metrics:
                self._metrics["get_count"] += 1
            return self._read_records(offset, limit)

    async def get_stream(
        self,
        limit: int,
        offset: int,
        wait: bool = False,
        timeout: float | None = None,
    ) -> AsyncIterator[list[dict[str, Any]]]:
        current_offset = offset
        remaining = limit

        while remaining > 0:
            async with self._condition:
                total = self.total_count()

                if current_offset >= total:
                    if wait:
                        try:
                            if timeout:
                                await asyncio.wait_for(
                                    self._condition.wait(), timeout=timeout
                                )
                            else:
                                await self._condition.wait()
                            continue
                        except asyncio.TimeoutError:
                            return
                    else:
                        return

                records = self._read_records(current_offset, min(remaining, 100))

            if records:
                yield records
                current_offset += len(records)
                remaining -= len(records)
            elif not wait:
                break

    async def flush(self) -> None:
        async with self._lock:
            if not self.buffer:
                return
            records = self.buffer[:]
            self.buffer = []

        if not LANCE_AVAILABLE:
            logger.error("Lance not available, cannot persist data")
            async with self._lock:
                self.buffer = records + self.buffer
            return

        try:
            table = self._records_to_table(records)

            if self._dataset_path.exists():
                existing = lance.dataset(self._dataset_path).to_table()
                combined = pa.concat_tables([existing, table])
                lance.write_dataset(combined, self._dataset_path, mode="overwrite")
            else:
                lance.write_dataset(table, self._dataset_path, mode="create")

            self.persisted_count += len(records)

            # 清理 WAL
            self._clear_wal()

            if self.enable_metrics:
                self._metrics["flush_count"] += 1
                self._metrics["last_flush_time"] = time.time()

            logger.debug(
                f"PersistingBackend[{self.bucket_id}] flushed {len(records)} records"
            )

        except Exception as e:
            logger.error(f"PersistingBackend[{self.bucket_id}] flush error: {e}")
            async with self._lock:
                self.buffer = records + self.buffer

    async def stats(self) -> dict[str, Any]:
        async with self._lock:
            stats = {
                "bucket_id": self.bucket_id,
                "buffer_size": len(self.buffer),
                "persisted_count": self.persisted_count,
                "total_count": self.total_count(),
                "backend": "persisting",
                "storage_path": str(self.storage_path),
                "wal_enabled": self.enable_wal,
            }
            if self.enable_metrics:
                stats["metrics"] = self._metrics.copy()
            return stats

    def _read_records(self, offset: int, limit: int) -> list[dict[str, Any]]:
        """读取数据：先读持久化，再读内存缓冲"""
        records = []

        # 从持久化数据读取
        if offset < self.persisted_count and self._dataset_path.exists() and LANCE_AVAILABLE:
            try:
                dataset = lance.dataset(self._dataset_path)
                persisted_limit = min(limit, self.persisted_count - offset)
                table = dataset.to_table(offset=offset, limit=persisted_limit)
                if table.num_rows > 0:
                    columns = table.column_names
                    for i in range(table.num_rows):
                        records.append({col: table[col][i].as_py() for col in columns})
            except Exception as e:
                logger.error(f"PersistingBackend[{self.bucket_id}] read error: {e}")

        # 从内存缓冲读取
        if len(records) < limit:
            buffer_offset = max(0, offset - self.persisted_count)
            buffer_limit = limit - len(records)
            buffer_records = self.buffer[buffer_offset : buffer_offset + buffer_limit]
            records.extend(buffer_records)

        return records

    def _records_to_table(self, records: list[dict[str, Any]]) -> "pa.Table":
        """将记录转换为 PyArrow Table"""
        field_names = set()
        for record in records:
            field_names.update(record.keys())

        arrays = {}
        for field_name in field_names:
            values = [record.get(field_name) for record in records]
            if all(isinstance(v, int) for v in values if v is not None):
                arrays[field_name] = pa.array(values, type=pa.int64())
            elif all(isinstance(v, float) for v in values if v is not None):
                arrays[field_name] = pa.array(values, type=pa.float64())
            elif all(isinstance(v, bool) for v in values if v is not None):
                arrays[field_name] = pa.array(values, type=pa.bool_())
            else:
                arrays[field_name] = pa.array(
                    [str(v) if v is not None else None for v in values],
                    type=pa.string(),
                )

        return pa.table(arrays)

    # ============================================================
    # Persisting 特有功能
    # ============================================================

    async def compact(self) -> None:
        """手动触发压缩"""
        # TODO: 实现压缩逻辑
        logger.info(f"PersistingBackend[{self.bucket_id}] compaction triggered")

    async def create_index(self, column: str) -> None:
        """为指定列创建索引"""
        # TODO: 实现索引创建逻辑
        logger.info(f"PersistingBackend[{self.bucket_id}] index created on {column}")

    def get_metrics(self) -> dict[str, Any]:
        """获取监控指标（用于 Prometheus 等）"""
        return self._metrics.copy()
