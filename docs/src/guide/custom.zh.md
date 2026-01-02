# 自定义后端

本指南展示如何为 Pulsing 队列实现自定义存储后端。

## StorageBackend 协议

所有后端必须实现 `StorageBackend` 协议：

```python
from typing import Protocol, AsyncIterator, Any

class StorageBackend(Protocol):
    """可插拔存储后端协议。"""
    
    async def put(self, record: dict[str, Any]) -> None:
        """存储单条记录。"""
        ...
    
    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        """高效存储多条记录。"""
        ...
    
    async def get(
        self,
        offset: int = 0,
        limit: int | None = None
    ) -> list[dict[str, Any]]:
        """从存储检索记录。"""
        ...
    
    async def get_stream(
        self,
        offset: int = 0,
        limit: int | None = None,
        block: bool = False
    ) -> AsyncIterator[dict[str, Any]]:
        """从存储流式读取记录。"""
        ...
    
    async def flush(self) -> None:
        """持久化任何缓冲数据。"""
        ...
    
    async def stats(self) -> dict[str, Any]:
        """返回存储统计信息。"""
        ...
    
    def total_count(self) -> int:
        """返回总记录数。"""
        ...
```

## 基本实现

这是一个最小的后端实现：

```python
import asyncio
from typing import Any, AsyncIterator

class SimpleBackend:
    """简单的内存后端。"""
    
    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        **kwargs,
    ):
        self.bucket_id = bucket_id
        self.storage_path = storage_path
        self.batch_size = batch_size
        self._records: list[dict[str, Any]] = []
        self._condition = asyncio.Condition()
    
    async def put(self, record: dict[str, Any]) -> None:
        """存储单条记录。"""
        async with self._condition:
            self._records.append(record)
            self._condition.notify_all()
    
    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        """存储多条记录。"""
        async with self._condition:
            self._records.extend(records)
            self._condition.notify_all()
    
    async def get(
        self,
        offset: int = 0,
        limit: int | None = None
    ) -> list[dict[str, Any]]:
        """检索记录。"""
        if limit is None:
            return self._records[offset:]
        return self._records[offset:offset + limit]
    
    async def get_stream(
        self,
        offset: int = 0,
        limit: int | None = None,
        block: bool = False
    ) -> AsyncIterator[dict[str, Any]]:
        """流式读取记录。"""
        current = offset
        count = 0
        
        while True:
            async with self._condition:
                while current >= len(self._records):
                    if not block:
                        return
                    await self._condition.wait()
                
                while current < len(self._records):
                    if limit is not None and count >= limit:
                        return
                    yield self._records[current]
                    current += 1
                    count += 1
    
    async def flush(self) -> None:
        """内存存储无操作。"""
        pass
    
    async def stats(self) -> dict[str, Any]:
        """返回统计信息。"""
        return {
            "bucket_id": self.bucket_id,
            "total_count": len(self._records),
            "buffer_count": 0,
            "persisted_count": len(self._records),
        }
    
    def total_count(self) -> int:
        """返回总记录数。"""
        return len(self._records)
```

## 注册自定义后端

```python
import pulsing as pul

# 注册后端
pul.queue.register_backend("simple", SimpleBackend)

# 使用它
writer = await pul.queue.write_queue(
    system,
    topic="my_topic",
    backend="simple",
)
```

## 高级实现

### 带持久化

```python
import json
from pathlib import Path

class FileBackend:
    """基于文件的持久化后端。"""
    
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
        self._buffer: list[dict[str, Any]] = []
        self._persisted_count = 0
        self._condition = asyncio.Condition()
        
        # 加载现有数据
        self._load()
    
    def _load(self) -> None:
        """加载持久化数据。"""
        self.storage_path.mkdir(parents=True, exist_ok=True)
        data_file = self.storage_path / "data.json"
        if data_file.exists():
            with open(data_file) as f:
                data = json.load(f)
                self._persisted_count = len(data)
    
    async def put(self, record: dict[str, Any]) -> None:
        async with self._condition:
            self._buffer.append(record)
            if len(self._buffer) >= self.batch_size:
                await self._flush_internal()
            self._condition.notify_all()
    
    async def _flush_internal(self) -> None:
        """内部刷新（无锁）。"""
        if not self._buffer:
            return
        
        data_file = self.storage_path / "data.json"
        
        # 加载现有数据
        existing = []
        if data_file.exists():
            with open(data_file) as f:
                existing = json.load(f)
        
        # 追加新数据
        existing.extend(self._buffer)
        
        # 写回
        with open(data_file, "w") as f:
            json.dump(existing, f)
        
        self._persisted_count += len(self._buffer)
        self._buffer.clear()
    
    async def flush(self) -> None:
        """持久化缓冲数据。"""
        async with self._condition:
            await self._flush_internal()
```

### 带阻塞支持

`get_stream` 方法在 `block=True` 时应等待新数据：

```python
async def get_stream(
    self,
    offset: int = 0,
    limit: int | None = None,
    block: bool = False
) -> AsyncIterator[dict[str, Any]]:
    """带阻塞支持的流式读取。"""
    current = offset
    count = 0
    
    while True:
        async with self._condition:
            # 如果阻塞则等待数据
            while current >= self.total_count():
                if not block:
                    return
                # 带超时等待以定期检查
                try:
                    await asyncio.wait_for(
                        self._condition.wait(),
                        timeout=1.0
                    )
                except asyncio.TimeoutError:
                    continue
            
            # 获取可用记录
            records = await self.get(current, limit)
            
        for record in records:
            if limit is not None and count >= limit:
                return
            yield record
            current += 1
            count += 1
```

## 测试自定义后端

```python
import pytest
import pulsing as pul

@pytest.fixture
def custom_backend():
    pul.queue.register_backend("custom", CustomBackend)
    yield
    # 如需清理

@pytest.mark.asyncio
async def test_custom_backend_write_read(actor_system, custom_backend):
    # 写入
    writer = await pul.queue.write_queue(
        actor_system,
        topic="test",
        backend="custom",
        storage_path="/tmp/test",
    )
    
    await writer.put({"id": "1", "value": 42})
    await writer.flush()
    
    # 读取
    reader = await pul.queue.read_queue(actor_system, "test")
    records = await reader.get(limit=10)
    
    assert len(records) == 1
    assert records[0]["id"] == "1"
```

## 最佳实践

1. **线程安全**：使用 `asyncio.Condition` 保证安全的并发访问
2. **缓冲**：缓冲写入以获得更好的性能
3. **错误处理**：优雅地处理存储错误
4. **统计信息**：提供有用的监控统计
5. **测试**：编写全面的测试

## 下一步

- [架构设计](../design/architecture.md) - 设计详情
- [API 参考](../api_reference.md) - 详细 API 文档

