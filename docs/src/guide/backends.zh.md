# 队列能力概览

`persisting.Queue` 是追加式持久化队列的统一 API。

## Queue API

```python
from persisting import Queue

queue = Queue("my_topic", storage_path="./data")
await queue.put({"id": "1", "value": 42})
await queue.flush()
records = await queue.get(limit=100)
```

## 启用指标

生产环境可观测时，可开启操作计数器：

```python
queue = Queue("my_topic", storage_path="./data", enable_metrics=True)
await queue.put({"id": "1", "value": 42})
await queue.flush()
stats = await queue.stats()
```

## 选项

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `storage_path` | `str` | `"./data"` | 队列数据根目录 |
| `batch_size` | `int` | `100` | 缓冲达到此数量时自动 flush |
| `enable_metrics` | `bool` | `False` | 收集操作计数器（put/get/flush 次数） |

## 对比

| 功能 | Queue（默认） | Queue（enable_metrics=True） |
|------|---------------|------------------------------|
| 持久化 | 是 | 是 |
| 重启恢复 | 是 | 是 |
| 自动 flush | 是 | 是 |
| 操作指标 | 否 | 是 |

## 实现

`Queue` 内部使用基于 Lance 的持久化存储：

- **列式存储** — 高效磁盘格式
- **内存缓冲** — 写入合并提升吞吐
- **自动 flush** — 缓冲达到 `batch_size` 时落盘

当 `enable_metrics=True` 时，使用扩展后端追踪 `put_count`、`get_count`、`flush_count` 和 `last_flush_time`。

## 下一步

- [Lance 后端](lance.md) — Queue 内部机制（缓冲、flush、存储）
- [自定义后端](custom.md) — 实现自己的后端供内部使用
