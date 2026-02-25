# 自定义后端

如何实现自定义存储后端。自定义后端需实现 `StorageBackend` 接口，可被队列系统内部使用。

## StorageBackend 协议

实现以下方法（定义在 `persisting.queue.backend`）：

```python
from typing import Any, AsyncIterator

class MyBackend:
    def __init__(self, bucket_id: int, storage_path: str, batch_size: int = 100, **kwargs):
        ...

    async def put(self, record: dict[str, Any]) -> None: ...
    async def put_batch(self, records: list[dict[str, Any]]) -> None: ...
    async def get(self, limit: int, offset: int) -> list[dict[str, Any]]: ...
    async def get_stream(self, limit: int, offset: int, wait: bool = False, timeout: float | None = None) -> AsyncIterator[list[dict[str, Any]]]: ...
    async def flush(self) -> None: ...
    async def stats(self) -> dict[str, Any]: ...
    def total_count(self) -> int: ...
```

构造函数必须接受 `bucket_id`、`storage_path` 和 `batch_size`，以及 `**kwargs` 以兼容未来新增参数。

## 最小示例

```python
import asyncio
from typing import Any, AsyncIterator

class SimpleBackend:
    def __init__(self, bucket_id: int, storage_path: str, batch_size: int = 100, **kwargs):
        self.bucket_id = bucket_id
        self._records: list[dict[str, Any]] = []
        self._condition = asyncio.Condition()

    def total_count(self) -> int:
        return len(self._records)

    async def put(self, record: dict[str, Any]) -> None:
        async with self._condition:
            self._records.append(record)
            self._condition.notify_all()

    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        async with self._condition:
            self._records.extend(records)
            self._condition.notify_all()

    async def get(self, limit: int, offset: int) -> list[dict[str, Any]]:
        return self._records[offset : offset + limit]

    async def get_stream(
        self, limit: int, offset: int, wait: bool = False, timeout: float | None = None,
    ) -> AsyncIterator[list[dict[str, Any]]]:
        pos = offset
        remaining = limit
        while remaining > 0:
            async with self._condition:
                if pos >= len(self._records):
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
                batch = self._records[pos : pos + min(remaining, 100)]
            if batch:
                yield batch
                pos += len(batch)
                remaining -= len(batch)
            elif not wait:
                break

    async def flush(self) -> None:
        pass  # 内存存储无需 flush

    async def stats(self) -> dict[str, Any]:
        return {"bucket_id": self.bucket_id, "total_count": len(self._records)}
```

实现此接口的自定义后端可在接受后端实例的地方内部使用。公开 API（`Queue`）使用基于 Lance 的存储；自定义后端适用于高级场景和框架集成。

## 要点

1. **使用 `asyncio.Condition`** 保证并发安全和阻塞读取。
2. **构造函数接受 `**kwargs`** 以兼容未来新增的选项。
3. **写入后调用 `notify_all()`** 让 `get_stream(wait=True)` 的读取端能被唤醒。
4. **`get_stream` 返回批次**（`list[dict]`），不是单条记录。

## 下一步

- [架构设计](../design/architecture.md) — 后端在系统中的位置
- [API 参考](../api_reference.md) — 完整 API 文档
