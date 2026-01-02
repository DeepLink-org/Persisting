# Write-Ahead Log (WAL)

本文档描述 Persisting 中的 Write-Ahead Log 设计。

## 概述

Write-Ahead Log (WAL) 通过在更改应用到主存储之前记录它们来确保数据持久性。这允许在崩溃后恢复未提交的数据。

## 设计目标

1. **持久性** - 确认写入后无数据丢失
2. **性能** - 写入操作的最小开销
3. **恢复** - 快速且正确的崩溃恢复
4. **简单性** - 易于理解和维护

## 架构

```
写入请求
     │
     ▼
┌─────────────────────────────────────────┐
│            WAL Writer                   │
│                                         │
│  1. 将记录序列化为字节                    │
│  2. 写入长度 + 校验和 + 数据              │
│  3. fsync() (如果是同步模式)              │
│                                         │
└────────────────┬────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────┐
│            WAL 文件                     │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │ 记录 1: [len][crc][data]        │   │
│  ├─────────────────────────────────┤   │
│  │ 记录 2: [len][crc][data]        │   │
│  ├─────────────────────────────────┤   │
│  │ 记录 3: [len][crc][data]        │   │
│  └─────────────────────────────────┘   │
│                                         │
└────────────────┬────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────┐
│           内存缓冲区                     │
│                                         │
│  (批量刷新的缓冲记录)                    │
│                                         │
└────────────────┬────────────────────────┘
                 │
                 ▼ flush()
┌─────────────────────────────────────────┐
│          Lance Dataset                  │
│                                         │
│  (持久化列式存储)                        │
│                                         │
└────────────────┬────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────┐
│          截断 WAL                       │
│                                         │
│  (删除已提交记录)                        │
│                                         │
└─────────────────────────────────────────┘
```

## 记录格式

每个 WAL 记录具有以下格式：

```
┌────────────┬────────────┬────────────────────┐
│   长度     │   校验和   │       数据         │
│  (4 字节)  │  (4 字节)  │    (可变)          │
└────────────┴────────────┴────────────────────┘
```

- **长度**: 记录数据长度 (uint32, 小端)
- **校验和**: 数据的 CRC32 (uint32, 小端)
- **数据**: 序列化的记录 (JSON 或 MessagePack)

## 写入路径

### 正常写入

```python
async def put(self, record: dict) -> None:
    if self.enable_wal:
        # 1. 首先写入 WAL
        await self._wal.append(record)
    
    # 2. 添加到内存缓冲区
    async with self._condition:
        self._buffer.append(record)
        self._condition.notify_all()
```

### 同步模式

| 模式 | 描述 | 持久性 | 性能 |
|------|------|--------|------|
| `sync` | 每次写入后 fsync | 最高 | 最慢 |
| `batch` | 定期 fsync | 高 | 快 |
| `async` | 操作系统控制同步 | 低 | 最快 |

```python
# 每次写入都同步（最安全）
backend_options={"wal_sync_interval": 0.0}

# 每 100ms 同步（平衡）
backend_options={"wal_sync_interval": 0.1}

# 操作系统控制（最快）
backend_options={"wal_sync_interval": 1.0}
```

## 恢复

### 启动恢复

```python
async def _recover(self) -> None:
    """从 WAL 恢复未提交的记录。"""
    if not self._wal_path.exists():
        return
    
    recovered = []
    async for record in self._wal.read_all():
        recovered.append(record)
    
    if recovered:
        logger.info(f"从 WAL 恢复了 {len(recovered)} 条记录")
        self._buffer.extend(recovered)
```

### 恢复流程

```
启动
    │
    ▼
┌─────────────────────────────────────────┐
│          检查 WAL 是否存在               │
│                                         │
│  WAL 路径: {storage_path}/wal/          │
│  WAL 文件: {bucket_id}.wal              │
│                                         │
└────────────────┬────────────────────────┘
                 │
        ┌────────┴────────┐
        │                 │
   WAL 存在           无 WAL
        │                 │
        ▼                 ▼
┌───────────────┐  ┌───────────────┐
│ 读取记录      │  │ 正常启动      │
│ 验证 CRC      │  │               │
│ 添加到缓冲区  │  │               │
└───────┬───────┘  └───────────────┘
        │
        ▼
┌───────────────┐
│ 继续处理      │
│ 恢复的数据    │
└───────────────┘
```

### 损坏处理

```python
async def _read_record(self, file) -> dict | None:
    """读取并验证单条记录。"""
    try:
        # 读取长度
        length_bytes = await file.read(4)
        if len(length_bytes) < 4:
            return None
        length = struct.unpack('<I', length_bytes)[0]
        
        # 读取校验和
        checksum_bytes = await file.read(4)
        checksum = struct.unpack('<I', checksum_bytes)[0]
        
        # 读取数据
        data = await file.read(length)
        
        # 验证校验和
        if crc32(data) != checksum:
            logger.warning("检测到 WAL 记录损坏")
            return None
        
        return json.loads(data)
    except Exception as e:
        logger.error(f"WAL 读取错误: {e}")
        return None
```

## 截断

成功刷新后截断 WAL：

```python
async def flush(self) -> None:
    """刷新缓冲区到 Lance 并截断 WAL。"""
    async with self._condition:
        if not self._buffer:
            return
        
        # 1. 写入 Lance
        await self._write_to_lance(self._buffer)
        
        # 2. 截断 WAL（记录现在已持久化）
        if self.enable_wal:
            await self._wal.truncate()
        
        # 3. 清空缓冲区
        self._buffer.clear()
```

## 文件管理

### WAL 目录结构

```
{storage_path}/
└── {topic}/
    └── bucket_{id}/
        ├── data.lance/          # Lance 数据集
        └── wal/
            ├── 0.wal           # 当前 WAL 文件
            └── 0.wal.tmp       # 轮转时的临时文件
```

### WAL 轮转

当 WAL 超过 `max_wal_size` 时：

```python
async def _maybe_rotate(self) -> None:
    """如果超过最大大小则轮转 WAL。"""
    if self._wal_size > self.max_wal_size:
        # 1. 强制刷新到 Lance
        await self.flush()
        
        # 2. 创建新 WAL 文件
        await self._wal.rotate()
        
        self._wal_size = 0
```

## 配置

| 选项 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `enable_wal` | bool | False | 启用 WAL |
| `wal_sync_interval` | float | 1.0 | 同步间隔 (0=每次写入) |
| `max_wal_size` | int | 100MB | 轮转前的最大 WAL 大小 |

## 性能考虑

### 写放大

每次写入都会写入 WAL 和缓冲区：
- WAL: 顺序追加（快）
- 缓冲区: 内存中（快）
- Lance: 刷新时批量写入（高效）

### 同步权衡

```
                    持久性
                        ▲
                        │
        sync 模式 ──────┤
                        │
       batch 模式 ──────┤
                        │
        async 模式 ─────┤
                        │
                        └──────────────────► 性能
```

### 最佳实践

1. **生产使用 batch 同步**: 平衡持久性和性能
2. **适当调整 WAL 大小**: 太小 = 频繁轮转，太大 = 恢复慢
3. **监控 WAL 大小**: 对意外增长发出警报
4. **测试恢复**: 定期验证恢复功能

## 限制

1. **单写入者假设**: WAL 假设每个 bucket 一个写入者
2. **有序恢复**: 记录按写入顺序恢复
3. **无部分记录恢复**: 跳过损坏的记录

## 未来改进

1. **压缩**: 压缩 WAL 记录
2. **异步 I/O**: 在 Linux 上使用 io_uring
3. **组提交**: 跨多个写入批量同步
4. **校验和块**: 更好地处理部分写入

