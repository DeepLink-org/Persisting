# 带指标的队列

通过 `enable_metrics=True` 开启操作指标，方便生产环境观测。

## 用法

```python
from persisting import Queue

queue = Queue("my_topic", storage_path="./data", enable_metrics=True)

await queue.put({"id": "1", "value": 42})
await queue.flush()
```

## 配置

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `enable_metrics` | `bool` | `False` | 是否收集操作计数器 |
| `batch_size` | `int` | `100` | 缓冲达到此数量时自动 flush |
| `auto_flush_interval_sec` | `float` | `0.0` | 大于 0 时定时 flush |

## 监控指标

| 指标 | 类型 | 说明 |
|------|------|------|
| `put_count` | int | 写入记录总数 |
| `get_count` | int | get() 调用次数 |
| `flush_count` | int | flush() 调用次数 |
| `last_flush_time` | float | 最近一次 flush 时间戳 |

### 查看指标

```python
stats = await queue.stats()
print(stats["metrics"])
```

## 对比

| | `Queue()` | `Queue(enable_metrics=True)` |
|-|-----------|------------------------------|
| 持久化 | 是 | 是 |
| 内存缓冲 | 是 | 是 |
| 自动 flush | 是 | 是 |
| 重启恢复 | 是 | 是 |
| 操作指标 | 否 | 是 |

## 下一步

- [Lance 后端](lance.md) — 存储引擎内部细节
- [自定义后端](custom.md) — 实现自己的后端
- [API 参考](../api_reference.md) — 完整 API 文档
