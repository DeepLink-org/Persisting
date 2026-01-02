# Lance 后端

Lance 后端使用 Lance 列式数据格式提供高性能持久化存储。

## 概述

Lance 是为 ML 工作流优化的现代列式数据格式。`LanceBackend` 为 Pulsing 队列带来以下能力：

- **高性能随机访问** - 高效检索任意记录
- **列式存储** - 分析查询的最佳选择
- **版本控制** - 内置数据版本管理
- **向量搜索** - 原生支持相似性搜索

## 安装

```bash
pip install persisting[pulsing]
```

## 基本用法

```python
import pulsing as pul
import persisting as pst

# 注册后端
pul.queue.register_backend("lance", pst.queue.LanceBackend)

# 创建写入器
writer = await pul.queue.write_queue(
    system,
    topic="my_topic",
    backend="lance",
    storage_path="/data/queues",
)

# 写入数据
await writer.put({"id": "1", "text": "Hello", "vector": [0.1, 0.2, 0.3]})
await writer.put({"id": "2", "text": "World", "vector": [0.4, 0.5, 0.6]})

# 刷新以持久化
await writer.flush()

# 读取数据
reader = await pul.queue.read_queue(system, "my_topic")
records = await reader.get(limit=10)
```

## 配置

### 后端选项

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `flush_threshold` | int | 1000 | 自动刷新前的记录数 |

### 配置示例

```python
import pulsing as pul

writer = await pul.queue.write_queue(
    system,
    topic="configured_topic",
    backend="lance",
    storage_path="/data/queues",
    batch_size=100,
    backend_options={
        "flush_threshold": 500,
    },
)
```

## 数据存储

### 目录结构

```
/data/queues/
└── my_topic/
    └── bucket_0/
        ├── data.lance/
        │   ├── _versions/
        │   │   ├── 1.manifest
        │   │   └── 2.manifest
        │   └── data/
        │       ├── 0.lance
        │       └── 1.lance
        └── ...
```

### Schema

Lance 后端从第一条记录自动推断 Schema：

```python
# 第一条记录定义 Schema
await writer.put({
    "id": "1",
    "text": "Hello",
    "value": 42,
    "vector": [0.1, 0.2, 0.3],
})

# 后续记录必须匹配 Schema
await writer.put({
    "id": "2",
    "text": "World",
    "value": 100,
    "vector": [0.4, 0.5, 0.6],
})
```

## 性能

### 缓冲

Lance 后端在持久化前将记录缓冲在内存中：

```python
# 记录被缓冲
for i in range(100):
    await writer.put({"id": str(i), "value": i})

# 显式刷新持久化到磁盘
await writer.flush()
```

### 自动刷新

达到 `flush_threshold` 时，数据自动持久化：

```python
import pulsing as pul

writer = await pul.queue.write_queue(
    system, "topic",
    backend="lance",
    backend_options={"flush_threshold": 500},
)

# 500 条记录后，自动刷新
for i in range(600):
    await writer.put({"id": str(i), "value": i})
```

## 流式处理

Lance 后端支持高效的流式处理：

```python
import pulsing as pul

reader = await pul.queue.read_queue(system, "my_topic")

# 非阻塞流
async for record in reader.get_stream(offset=0, limit=100):
    process(record)

# 阻塞流（等待新数据）
async for record in reader.get_stream(block=True):
    process(record)
```

## 统计信息

获取存储统计：

```python
stats = await writer.stats()
print(f"总记录数: {stats['total_count']}")
print(f"缓冲中: {stats['buffer_count']}")
print(f"已持久化: {stats['persisted_count']}")
```

## 最佳实践

1. **选择合适的批次大小** - 更大的批次提高写入吞吐量
2. **定期刷新** - 调用 `flush()` 确保数据持久性
3. **使用合适的存储路径** - 生产环境使用快速 SSD 存储
4. **监控缓冲区大小** - 避免大缓冲区造成内存压力

## 下一步

- [Persisting 后端](persisting.md) - 带 WAL 的增强功能
- [自定义后端](custom.md) - 实现自己的后端
- [API 参考](../api_reference.md) - 详细 API 文档

