# API 参考

Persisting 为 Pulsing 扩展分布式分层内存。本页面记录当前已可用的 API。Tensor Memory API（`persisting.open`、`Handler`、分层访问）正在开发中——规范请参阅[设计文档](design/index.md)。

## Queue

高层持久化队列，由 Lance 存储引擎支撑。所有队列操作应使用此类。

### 构造函数

```python
from persisting import Queue

queue = Queue(
    name: str,
    storage_path: str = "./data",
    *,
    batch_size: int = 100,
    auto_flush_interval_sec: float = 0.0,
    enable_metrics: bool = False,
)
```

- `name`: 队列名称（主题）— 用作 `storage_path` 下的子目录。
- `storage_path`: 队列数据根目录。
- `batch_size`: 缓冲达到此大小时自动 flush。
- `auto_flush_interval_sec`: 若 > 0，按此间隔（秒）flush。
- `enable_metrics`: 收集操作计数器（put/get/flush）。

### 方法

```python
# 写入
await queue.put({"id": "1", "value": 42})
await queue.put_batch([{"id": "2", "value": 100}, ...])
await queue.flush()

# 读取
records = await queue.get(limit=100, offset=0)

# 统计
stats = await queue.stats()
len(queue)  # total_count
queue.close()
```

### 示例

```python
from persisting import Queue

queue = Queue("my_topic", storage_path="./data")
await queue.put({"id": "1", "value": 42})
await queue.flush()
records = await queue.get(limit=100)
```

---

## 内部后端 (persisting.queue)

这些后端为 `persisting.Queue` 提供底层实现。大多数用户应直接使用 `Queue`。

### LanceBackend

基于 Lance 的持久化存储后端。

```python
class LanceBackend:
    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        **kwargs,
    ) -> None
```

**参数：**

| 名称 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `bucket_id` | `int` | — | 桶标识符（由 Pulsing 分配） |
| `storage_path` | `str` | — | Lance 数据集存储目录 |
| `batch_size` | `int` | `100` | 缓冲达到此数量时自动 flush |

**方法：**

| 方法 | 说明 |
|------|------|
| `put(record)` | 写入单条记录 |
| `put_batch(records)` | 批量写入 |
| `get(limit, offset)` | 读取记录（持久化 + 缓冲） |
| `get_stream(limit, offset, wait, timeout)` | 流式读取，`wait=True` 时阻塞等待新数据 |
| `flush()` | 将缓冲区数据落盘到 Lance |
| `stats()` | 返回存储统计信息 |
| `total_count()` | 返回总记录数 |

---

### PersistingBackend

继承 `LanceBackend`，增加操作指标。

```python
class PersistingBackend(LanceBackend):
    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        enable_metrics: bool = True,
        **kwargs,
    ) -> None
```

**额外参数：**

| 名称 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `enable_metrics` | `bool` | `True` | 是否收集操作计数器 |

**额外方法：**

| 方法 | 说明 |
|------|------|
| `get_metrics()` | 返回指标快照 |

指标字段：`put_count`、`get_count`、`flush_count`、`last_flush_time`。

---

## StorageBackend 协议

后端实现（LanceBackend、PersistingBackend）遵循此协议：

```python
class StorageBackend(Protocol):
    async def put(self, record: dict[str, Any]) -> None: ...
    async def put_batch(self, records: list[dict[str, Any]]) -> None: ...
    async def get(self, limit: int, offset: int) -> list[dict[str, Any]]: ...
    async def get_stream(self, limit: int, offset: int, wait: bool = False, timeout: float | None = None) -> AsyncIterator[list[dict[str, Any]]]: ...
    async def flush(self) -> None: ...
    async def stats(self) -> dict[str, Any]: ...
    def total_count(self) -> int: ...
```
