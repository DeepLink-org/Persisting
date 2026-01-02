# 队列后端

本指南概述 Persisting 中可用的存储后端。

## 后端协议

所有后端实现 `StorageBackend` 协议：

```python
from typing import Protocol, AsyncIterator, Any

class StorageBackend(Protocol):
    """可插拔存储后端协议。"""
    
    async def put(self, record: dict[str, Any]) -> None:
        """存储单条记录。"""
        ...
    
    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        """存储多条记录。"""
        ...
    
    async def get(self, offset: int = 0, limit: int | None = None) -> list[dict[str, Any]]:
        """检索记录。"""
        ...
    
    async def get_stream(
        self,
        offset: int = 0,
        limit: int | None = None,
        block: bool = False
    ) -> AsyncIterator[dict[str, Any]]:
        """流式读取记录。"""
        ...
    
    async def flush(self) -> None:
        """持久化缓冲数据。"""
        ...
    
    async def stats(self) -> dict[str, Any]:
        """获取存储统计信息。"""
        ...
    
    def total_count(self) -> int:
        """获取总记录数。"""
        ...
```

## 可用后端

### MemoryBackend (Pulsing)

无持久化的简单内存存储：

```python
import pulsing as pul

writer = await pul.queue.write_queue(system, "topic", backend="memory")
```

**特性：**

- ✅ 快速读写
- ✅ 无依赖
- ❌ 无持久化
- ❌ 重启后数据丢失

**用途：** 测试、开发、临时数据

### LanceBackend (Persisting)

基于 Lance 的持久化存储：

```python
import pulsing as pul
import persisting as pst

pul.queue.register_backend("lance", pst.queue.LanceBackend)
writer = await pul.queue.write_queue(system, "topic", backend="lance", storage_path="/data")
```

**特性：**

- ✅ 高性能列式存储
- ✅ 数据持久化
- ✅ 版本控制
- ✅ 向量搜索支持

**用途：** 生产工作负载、ML 数据存储

### PersistingBackend (Persisting)

具有高级功能的增强后端：

```python
import pulsing as pul
import persisting as pst

pul.queue.register_backend("persisting", pst.queue.PersistingBackend)
writer = await pul.queue.write_queue(
    system, "topic",
    backend="persisting",
    storage_path="/data",
    backend_options={"enable_wal": True}
)
```

**特性：**

- ✅ 所有 Lance 功能
- ✅ Write-Ahead Log (WAL)
- ✅ Schema 演进
- ✅ Prometheus 指标

**用途：** 有持久性要求的生产环境

## 后端比较

| 功能 | Memory | Lance | Persisting |
|------|--------|-------|------------|
| 持久化 | ❌ | ✅ | ✅ |
| WAL | ❌ | ❌ | ✅ |
| 压缩 | ❌ | ✅ | ✅ |
| 指标 | ❌ | ❌ | ✅ |
| Schema 演进 | N/A | ❌ | ✅ |
| 向量搜索 | ❌ | ✅ | ✅ |

## 后端注册

向 Pulsing 注册自定义后端：

```python
import pulsing as pul

# 注册后端
pul.queue.register_backend("my_backend", MyBackendClass)

# 获取已注册的后端
backend_class = pul.queue.get_backend_class("my_backend")

# 列出所有后端
available = pul.queue.list_backends()
print(available)  # ['memory', 'my_backend', ...]
```

## 下一步

- [Lance 后端](lance.md) - 详细的 Lance 后端指南
- [Persisting 后端](persisting.md) - 增强后端功能
- [自定义后端](custom.md) - 实现自己的后端

