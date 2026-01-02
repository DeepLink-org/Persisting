# Persisting

Persistent Storage for Parameters, KV Cache, and Trajectories

## 特性

- **Lance 持久化后端**：为 Pulsing 队列系统提供持久化存储
- **WAL 支持**：Write-Ahead Log 用于故障恢复（PersistingBackend）
- **监控指标**：内置指标导出

## 安装

```bash
pip install persisting

# 包含 Lance 支持
pip install persisting[lance]
```

## 快速开始

### 与 Pulsing 队列配合使用

```python
import asyncio
from pulsing.actor import SystemConfig, create_actor_system
from pulsing.queue import write_queue, read_queue, register_backend
from persisting.queue import LanceBackend, PersistingBackend

async def main():
    system = await create_actor_system(SystemConfig.standalone())
    
    # 注册 Lance 后端
    register_backend("lance", LanceBackend)
    
    # 使用 Lance 后端（持久化）
    writer = await write_queue(
        system, 
        "my_queue",
        backend="lance",
        storage_path="./data/my_queue"
    )
    
    # 写入数据
    await writer.put({"id": "1", "value": 100})
    await writer.flush()  # 持久化到磁盘
    
    # 读取数据
    reader = await read_queue(
        system, "my_queue", 
        backend="lance",
        storage_path="./data/my_queue"
    )
    records = await reader.get(limit=100)
    
    await system.shutdown()

asyncio.run(main())
```

### 使用增强版后端

```python
from persisting.queue import PersistingBackend

register_backend("persisting", PersistingBackend)

writer = await write_queue(
    system, 
    "my_queue",
    backend="persisting",
    backend_options={
        "enable_wal": True,       # 启用 WAL
        "enable_metrics": True,    # 启用监控指标
    }
)

# 获取统计信息（包含监控指标）
stats = await writer.queue.stats()
print(stats["metrics"])
```

## 存储后端对比

| 后端 | 来源 | 持久化 | WAL | 监控 | 适用场景 |
|------|------|--------|-----|------|----------|
| `MemoryBackend` | pulsing（内置） | ❌ | ❌ | ❌ | 测试 |
| `LanceBackend` | persisting | ✅ | ❌ | ❌ | 一般持久化 |
| `PersistingBackend` | persisting | ✅ | ✅ | ✅ | 生产环境 |

## 架构

```
┌─────────────────────────────────────────────────────────────────┐
│                      用户代码                                    │
│   writer = await write_queue(system, "topic", backend="lance")  │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Pulsing Queue API                            │
│   QueueWriter / QueueReader / StorageManager                    │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│              StorageBackend Protocol (可插拔)                    │
└─────────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
┌───────────────┐    ┌───────────────┐    ┌───────────────────┐
│ MemoryBackend │    │ LanceBackend  │    │ PersistingBackend │
│ (pulsing)     │    │ (persisting)  │    │ (persisting)      │
│               │    │               │    │                   │
│ - 纯内存      │    │ - Lance 持久化│    │ - Lance 持久化    │
│ - 无持久化    │    │               │    │ - WAL 支持        │
│               │    │               │    │ - 监控指标        │
└───────────────┘    └───────────────┘    └───────────────────┘
```

## 自定义后端

实现 `StorageBackend` 协议即可创建自定义后端：

```python
from typing import Any, AsyncIterator

class MyCustomBackend:
    def __init__(self, bucket_id: int, storage_path: str, **kwargs):
        self.bucket_id = bucket_id
        # ...
    
    async def put(self, record: dict[str, Any]) -> None: ...
    async def put_batch(self, records: list[dict[str, Any]]) -> None: ...
    async def get(self, limit: int, offset: int) -> list[dict[str, Any]]: ...
    async def get_stream(self, limit, offset, wait, timeout) -> AsyncIterator[list[dict]]: ...
    async def flush(self) -> None: ...
    async def stats(self) -> dict[str, Any]: ...
    def total_count(self) -> int: ...
```

## 路线图

- [x] LanceBackend 基础实现
- [x] PersistingBackend 框架
- [ ] WAL 完整实现
- [ ] Schema 演化
- [ ] 数据压缩优化
- [ ] 索引支持
- [ ] Prometheus 指标导出
- [ ] KV Cache 存储
- [ ] Parameter 存储
- [ ] Trajectory 存储

## 许可证

Apache-2.0
