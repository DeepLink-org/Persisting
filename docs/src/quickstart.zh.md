# 快速开始

本指南将帮助你在几分钟内开始使用 Persisting。

## 基本用法

### 步骤 1：安装 Persisting

```bash
pip install persisting[pulsing]
```

### 步骤 2：向 Pulsing 注册后端

```python
import pulsing as pul
import persisting as pst

# 注册 Lance 后端
pul.queue.register_backend("lance", pst.queue.LanceBackend)
```

### 步骤 3：创建持久化队列

```python
import asyncio
import pulsing as pul
import persisting as pst

async def main():
    # 注册后端
    pul.queue.register_backend("lance", pst.queue.LanceBackend)
    
    # 创建 Actor 系统
    system = await pul.actor.create_actor_system(pul.actor.SystemConfig.standalone())
    
    # 创建使用 Lance 持久化的队列写入器
    writer = await pul.queue.write_queue(
        system,
        topic="my_data",
        backend="lance",
        storage_path="/data/queues",
        num_buckets=4,
        batch_size=100,
    )
    
    # 写入数据
    for i in range(10):
        await writer.put({"id": str(i), "value": i * 10})
    
    # 刷新以持久化
    await writer.flush()
    
    # 创建队列读取器
    reader = await pul.queue.read_queue(system, "my_data")
    
    # 读取数据
    async for record in reader.get_stream():
        print(f"读取: {record}")
    
    await system.shutdown()

asyncio.run(main())
```

## 使用不同的后端

### 内存后端（用于测试）

```python
import pulsing as pul

# 内存后端内置于 Pulsing（无持久化）
writer = await pul.queue.write_queue(
    system,
    topic="test_topic",
    backend="memory",
)
```

### Lance 后端（推荐用于生产）

```python
import pulsing as pul
import persisting as pst

pul.queue.register_backend("lance", pst.queue.LanceBackend)

writer = await pul.queue.write_queue(
    system,
    topic="prod_topic",
    backend="lance",
    storage_path="/data/queues",
)
```

### Persisting 后端（增强功能）

```python
import pulsing as pul
import persisting as pst

pul.queue.register_backend("persisting", pst.queue.PersistingBackend)

writer = await pul.queue.write_queue(
    system,
    topic="advanced_topic",
    backend="persisting",
    storage_path="/data/queues",
    backend_options={
        "enable_wal": True,
        "compression": "zstd",
    },
)
```

## 后端选项

### Lance 后端选项

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `flush_threshold` | int | 1000 | 自动刷新前的记录数 |

### Persisting 后端选项

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `enable_wal` | bool | False | 启用 Write-Ahead Log |
| `compression` | str | None | 压缩算法 |
| `enable_metrics` | bool | False | 启用 Prometheus 指标 |

## 下一步

- [用户指南](guide/index.md) - 了解更多关于存储后端
- [Lance 后端](guide/lance.md) - Lance 后端详情
- [Persisting 后端](guide/persisting.md) - 增强后端功能
- [API 参考](api_reference.md) - 详细 API 文档

