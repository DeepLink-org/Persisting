# 架构设计

本文档描述 Persisting 的整体架构。

## 系统组件

### 1. StorageBackend 协议

所有后端实现的核心抽象：

```python
class StorageBackend(Protocol):
    """可插拔持久化的存储后端协议。"""
    
    async def put(self, record: dict[str, Any]) -> None:
        """存储单条记录。"""
    
    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        """高效存储多条记录。"""
    
    async def get(self, offset: int, limit: int | None) -> list[dict]:
        """从偏移量检索记录。"""
    
    async def get_stream(
        self, offset: int, limit: int | None, block: bool
    ) -> AsyncIterator[dict]:
        """流式读取记录，可选阻塞等待新数据。"""
    
    async def flush(self) -> None:
        """将缓冲数据持久化到存储。"""
    
    async def stats(self) -> dict[str, Any]:
        """返回存储统计信息。"""
    
    def total_count(self) -> int:
        """返回总记录数。"""
```

### 2. LanceBackend

使用 Lance 列式格式的高性能存储：

```
┌─────────────────────────────────────────┐
│             LanceBackend                │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │          内存缓冲区              │   │
│  │    (等待刷新的记录)              │   │
│  └──────────────┬──────────────────┘   │
│                 │                       │
│                 ▼ flush()               │
│  ┌─────────────────────────────────┐   │
│  │        Lance Dataset            │   │
│  │    (磁盘上的列式存储)            │   │
│  └─────────────────────────────────┘   │
│                                         │
└─────────────────────────────────────────┘
```

特性：
- 内存缓冲用于写合并
- 自动 Schema 推断
- 内置版本控制
- 高性能列式存储

### 3. PersistingBackend

带企业级功能的增强后端：

```
┌─────────────────────────────────────────────────────────┐
│                   PersistingBackend                      │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │                  WAL 层                          │   │
│  │          (用于持久性的预写日志)                   │   │
│  └──────────────────────┬──────────────────────────┘   │
│                         │                               │
│                         ▼                               │
│  ┌─────────────────────────────────────────────────┐   │
│  │                内存缓冲区                        │   │
│  │            (支持压缩)                           │   │
│  └──────────────────────┬──────────────────────────┘   │
│                         │                               │
│                         ▼ flush()                       │
│  ┌─────────────────────────────────────────────────┐   │
│  │              Lance Dataset                       │   │
│  │          (支持 Schema 演进)                      │   │
│  └─────────────────────────────────────────────────┘   │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │              指标收集器                          │   │
│  │        (Prometheus 指标导出)                    │   │
│  └─────────────────────────────────────────────────┘   │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

特性：
- 所有 LanceBackend 功能
- 用于崩溃恢复的预写日志
- Schema 演进支持
- 压缩选项
- Prometheus 指标

## 数据流

### 写入路径

```
应用程序
    │
    ▼ put(record)
┌─────────────────┐
│  QueueWriter    │
└────────┬────────┘
         │
         ▼ 按 bucket_column 路由
┌─────────────────┐
│ StorageManager  │
└────────┬────────┘
         │
         ▼ 发送到 bucket 所有者
┌─────────────────┐
│ BucketStorage   │
└────────┬────────┘
         │
         ▼ 委托给后端
┌─────────────────┐
│ StorageBackend  │──► WAL (如果启用)
└────────┬────────┘
         │
         ▼ 缓冲
┌─────────────────┐
│   内存缓冲区     │
└────────┬────────┘
         │
         ▼ 达到阈值时 flush()
┌─────────────────┐
│  Lance Dataset  │
└─────────────────┘
```

### 读取路径

```
应用程序
    │
    ▼ get(offset, limit)
┌─────────────────┐
│  QueueReader    │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ StorageManager  │
└────────┬────────┘
         │
         ▼ 查询每个 bucket
┌─────────────────────────────────────┐
│      BucketStorage (并行)           │
│   ┌─────┐  ┌─────┐  ┌─────┐       │
│   │ B0  │  │ B1  │  │ BN  │       │
│   └──┬──┘  └──┬──┘  └──┬──┘       │
│      │        │        │           │
│      ▼        ▼        ▼           │
│   StorageBackend.get()             │
│      │        │        │           │
│      └────────┼────────┘           │
│               │                     │
│               ▼                     │
│          合并结果                   │
└─────────────────────────────────────┘
         │
         ▼
    应用程序
```

## Bucket 分布

记录通过一致性哈希分布到各 bucket：

```
┌──────────────────────────────────────────────────────────┐
│                    Bucket 环                             │
│                                                          │
│                       节点 A                             │
│                    ┌─────────┐                          │
│              ┌─────│ Bucket 0│─────┐                    │
│              │     └─────────┘     │                    │
│              │                     │                    │
│        ┌─────┴───┐           ┌────┴────┐               │
│   节点 C│Bucket 3│           │Bucket 1 │节点 B          │
│        └─────┬───┘           └────┬────┘               │
│              │                    │                     │
│              │     ┌─────────┐    │                     │
│              └─────│ Bucket 2│────┘                     │
│                    └─────────┘                          │
│                       节点 B                             │
│                                                          │
└──────────────────────────────────────────────────────────┘

记录路由:
  hash(record[bucket_column]) % num_buckets → bucket_id
  bucket_id → owner_node (通过一致性哈希)
```

## 并发模型

### 线程安全

所有后端使用 `asyncio.Condition` 实现安全的并发访问：

```python
class Backend:
    def __init__(self):
        self._condition = asyncio.Condition()
    
    async def put(self, record):
        async with self._condition:
            self._buffer.append(record)
            self._condition.notify_all()  # 唤醒读取者
    
    async def get_stream(self, block=False):
        async with self._condition:
            while no_data_available:
                if not block:
                    return
                await self._condition.wait()  # 等待数据
```

### 阻塞读取

`get_stream(block=True)` 模式支持消费者阻塞：

```python
async for record in reader.get_stream(block=True):
    # 追上时会等待新记录
    process(record)
```

## 配置

### 后端选择

```python
# 按名称（必须已注册）
writer = await write_queue(system, topic, backend="lance")

# 按类（自动注册）
writer = await write_queue(system, topic, backend=LanceBackend)
```

### 后端选项

选项传递给后端构造函数：

```python
writer = await write_queue(
    system, topic,
    backend="persisting",
    backend_options={
        "enable_wal": True,
        "compression": "zstd",
    },
)
```

## 错误处理

### 写入失败

```python
try:
    await writer.put(record)
except StorageError as e:
    # 处理存储失败
    logger.error(f"写入失败: {e}")
```

### 恢复

启用 WAL 后，未提交的数据在启动时恢复：

```
启动
    │
    ▼
检查 WAL 文件
    │
    ├─► 无 WAL: 正常启动
    │
    └─► WAL 存在: 重放未提交记录
            │
            ▼
        应用到缓冲区
            │
            ▼
        继续正常操作
```

## 未来考虑

1. **多区域复制** - 跨数据中心数据同步
2. **分层存储** - 热/冷数据管理
3. **查询下推** - 存储层过滤计算
4. **备份/恢复** - 时间点恢复

