# API 参考

本文档提供 Persisting 的详细 API 参考。

## persisting.queue

### LanceBackend

使用 Lance 列式格式的高性能存储后端。

```python
class LanceBackend:
    """基于 Lance 的 Pulsing 队列存储后端。"""
    
    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        flush_threshold: int = 1000,
        **kwargs,
    ) -> None:
        """
        初始化 Lance 后端。
        
        参数
        ----------
        bucket_id : int
            此 bucket 的唯一标识符。
        storage_path : str
            存储 Lance 数据集的路径。
        batch_size : int, 可选
            操作的批次大小（默认: 100）。
        flush_threshold : int, 可选
            自动刷新前的记录数（默认: 1000）。
        **kwargs
            其他选项（忽略）。
        """
```

#### 方法

##### put

```python
async def put(self, record: dict[str, Any]) -> None:
    """
    存储单条记录。
    
    参数
    ----------
    record : dict[str, Any]
        要存储的记录。
    """
```

##### put_batch

```python
async def put_batch(self, records: list[dict[str, Any]]) -> None:
    """
    高效存储多条记录。
    
    参数
    ----------
    records : list[dict[str, Any]]
        要存储的记录。
    """
```

##### get

```python
async def get(
    self,
    offset: int = 0,
    limit: int | None = None,
) -> list[dict[str, Any]]:
    """
    从存储检索记录。
    
    参数
    ----------
    offset : int, 可选
        起始偏移量（默认: 0）。
    limit : int | None, 可选
        返回的最大记录数（默认: None = 全部）。
    
    返回
    -------
    list[dict[str, Any]]
        检索到的记录。
    """
```

##### get_stream

```python
async def get_stream(
    self,
    offset: int = 0,
    limit: int | None = None,
    block: bool = False,
) -> AsyncIterator[dict[str, Any]]:
    """
    从存储流式读取记录。
    
    参数
    ----------
    offset : int, 可选
        起始偏移量（默认: 0）。
    limit : int | None, 可选
        返回的最大记录数（默认: None = 无限制）。
    block : bool, 可选
        如果为 True，追上时等待新记录（默认: False）。
    
    生成
    ------
    dict[str, Any]
        来自存储的记录。
    """
```

##### flush

```python
async def flush(self) -> None:
    """
    将缓冲数据持久化到 Lance 存储。
    
    将所有缓冲记录写入 Lance 数据集并清空缓冲区。
    """
```

##### stats

```python
async def stats(self) -> dict[str, Any]:
    """
    获取存储统计信息。
    
    返回
    -------
    dict[str, Any]
        统计信息包括:
        - bucket_id: Bucket 标识符
        - total_count: 总记录数
        - buffer_count: 缓冲区中的记录数
        - persisted_count: Lance 中的记录数
        - storage_path: Lance 数据集路径
    """
```

##### total_count

```python
def total_count(self) -> int:
    """
    获取总记录数。
    
    返回
    -------
    int
        总记录数（缓冲区 + 已持久化）。
    """
```

---

### PersistingBackend

带 WAL、压缩和指标的增强存储后端。

```python
class PersistingBackend:
    """带企业级功能的增强存储后端。"""
    
    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        enable_wal: bool = False,
        compression: str | None = None,
        enable_metrics: bool = False,
        wal_sync_interval: float = 1.0,
        max_wal_size: int = 100 * 1024 * 1024,
        **kwargs,
    ) -> None:
        """
        初始化 Persisting 后端。
        
        参数
        ----------
        bucket_id : int
            此 bucket 的唯一标识符。
        storage_path : str
            存储数据的路径。
        batch_size : int, 可选
            操作的批次大小（默认: 100）。
        enable_wal : bool, 可选
            启用 Write-Ahead Log（默认: False）。
        compression : str | None, 可选
            压缩算法: "zstd", "lz4", "snappy"（默认: None）。
        enable_metrics : bool, 可选
            启用 Prometheus 指标（默认: False）。
        wal_sync_interval : float, 可选
            WAL 同步间隔（秒）（默认: 1.0）。
        max_wal_size : int, 可选
            最大 WAL 文件大小（字节）（默认: 100MB）。
        **kwargs
            其他选项（忽略）。
        """
```

#### 方法

继承 `LanceBackend` 的所有方法，并增强行为：

- `put()` - 如果启用则首先写入 WAL
- `flush()` - 成功刷新后截断 WAL
- `stats()` - 包含 WAL 和压缩信息

---

## 使用示例

### 基本用法

```python
import pulsing as pul
import persisting as pst

# 注册后端
pul.queue.register_backend("lance", pst.queue.LanceBackend)

# 创建写入器
writer = await pul.queue.write_queue(
    system,
    topic="example",
    backend="lance",
    storage_path="/data/queues",
)

# 写入数据
await writer.put({"id": "1", "value": 42})
await writer.flush()

# 读取数据
reader = await pul.queue.read_queue(system, "example")
records = await reader.get(limit=10)
```

### 使用 WAL

```python
import pulsing as pul
import persisting as pst

pul.queue.register_backend("persisting", pst.queue.PersistingBackend)

writer = await pul.queue.write_queue(
    system,
    topic="durable",
    backend="persisting",
    backend_options={
        "enable_wal": True,
        "wal_sync_interval": 0.1,
    },
)
```

### 流式处理

```python
import pulsing as pul

reader = await pul.queue.read_queue(system, "example")

# 非阻塞流
async for record in reader.get_stream(limit=100):
    process(record)

# 阻塞流（等待新数据）
async for record in reader.get_stream(block=True):
    process(record)
```

---

## 异常

### StorageError

存储操作的基础异常。

```python
class StorageError(Exception):
    """存储操作的基础异常。"""
    pass
```

### WALError

WAL 相关错误的异常。

```python
class WALError(StorageError):
    """WAL 操作的异常。"""
    pass
```

---

## 类型定义

### StorageBackend 协议

```python
from typing import Protocol, AsyncIterator, Any

class StorageBackend(Protocol):
    """可插拔存储后端的协议。"""
    
    async def put(self, record: dict[str, Any]) -> None: ...
    async def put_batch(self, records: list[dict[str, Any]]) -> None: ...
    async def get(self, offset: int = 0, limit: int | None = None) -> list[dict[str, Any]]: ...
    async def get_stream(
        self, offset: int = 0, limit: int | None = None, block: bool = False
    ) -> AsyncIterator[dict[str, Any]]: ...
    async def flush(self) -> None: ...
    async def stats(self) -> dict[str, Any]: ...
    def total_count(self) -> int: ...
```

