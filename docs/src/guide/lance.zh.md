# Lance 后端

`Queue` 内部使用基于 Lance 的持久化存储。本页说明其机制。

## 概述

[Lance](https://github.com/lancedb/lance) 是面向 ML 工作流优化的列式数据格式。`Queue` 基于 Lance 实现持久化：

- **列式存储** — 高效磁盘格式
- **内存缓冲** — 写入合并提升吞吐
- **自动 flush** — 缓冲达到 `batch_size` 时自动落盘
- **重启恢复** — 启动时恢复已持久化的记录计数

## 安装

```bash
pip install persisting[lance]
```

## 用法

```python
from persisting import Queue

queue = Queue("my_topic", storage_path="./data/queues")

# 写入
await queue.put({"id": "1", "text": "Hello", "value": 42})
await queue.flush()

# 读取
records = await queue.get(limit=10)
```

## 配置

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `batch_size` | `int` | `100` | 缓冲达到此数量时自动 flush |
| `storage_path` | `str` | — | 队列数据根目录（队列名作为子目录） |

## 类型推断

| Python 类型 | Arrow 类型 |
|-------------|-----------|
| `int` | `int64` |
| `float` | `float64` |
| `bool` | `bool` |
| 其他 | `string` |

## 缓冲与 flush

记录在内存缓冲中累积。flush 在以下情况触发：

1. 缓冲达到 `batch_size`（自动 flush）
2. 显式调用 `flush()`

```python
queue = Queue("topic", storage_path="./data")

for i in range(100):
    await queue.put({"id": str(i), "value": i})
await queue.flush()
```

## 统计信息

```python
stats = await queue.stats()
# {
#     "bucket_id": 0,
#     "backend": "lance",
#     "storage_path": "./data/my_topic",
#     "buffer_size": 42,
#     "persisted_count": 1000,
#     "total_count": 1042,
# }
```

## 下一步

- [自定义后端](custom.md) — 实现自己的后端
- [API 参考](../api_reference.md) — 完整 API 文档
