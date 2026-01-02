# Persisting 后端

Persisting 后端扩展了 Lance 后端，为生产部署提供企业级功能。

## 概述

`PersistingBackend` 提供所有 Lance 功能，外加：

- **Write-Ahead Log (WAL)** - 崩溃恢复和数据持久性
- **Schema 演进** - 无停机动态 Schema 变更
- **压缩** - 多种压缩算法
- **监控** - Prometheus 指标导出

## 安装

```bash
pip install persisting[pulsing]
```

## 基本用法

```python
import pulsing as pul
import persisting as pst

# 注册后端
pul.queue.register_backend("persisting", pst.queue.PersistingBackend)

# 创建带增强功能的写入器
writer = await pul.queue.write_queue(
    system,
    topic="production_topic",
    backend="persisting",
    storage_path="/data/queues",
    backend_options={
        "enable_wal": True,
        "compression": "zstd",
        "enable_metrics": True,
    },
)

# 写入数据
await writer.put({"id": "1", "value": 42})
await writer.flush()
```

## 配置

### 后端选项

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `enable_wal` | bool | False | 启用 Write-Ahead Log |
| `compression` | str | None | 压缩: "zstd", "lz4", "snappy" |
| `enable_metrics` | bool | False | 启用 Prometheus 指标 |
| `wal_sync_interval` | float | 1.0 | WAL 同步间隔（秒） |
| `max_wal_size` | int | 100MB | 最大 WAL 文件大小 |

### 完整配置示例

```python
import pulsing as pul

writer = await pul.queue.write_queue(
    system,
    topic="advanced_topic",
    backend="persisting",
    storage_path="/data/queues",
    batch_size=100,
    backend_options={
        "enable_wal": True,
        "compression": "zstd",
        "enable_metrics": True,
        "wal_sync_interval": 0.5,
        "max_wal_size": 50 * 1024 * 1024,  # 50MB
    },
)
```

## Write-Ahead Log (WAL)

### 概述

WAL 通过在应用更改前将其写入日志来确保数据持久性：

```
写入请求 → WAL → 缓冲区 → Lance 存储
            ↓
       (崩溃恢复)
```

### 启用 WAL

```python
import pulsing as pul

writer = await pul.queue.write_queue(
    system, "topic",
    backend="persisting",
    backend_options={"enable_wal": True},
)
```

### 恢复

启动时，后端自动从 WAL 恢复未提交的数据：

```python
import pulsing as pul

# 崩溃后，数据自动恢复
writer = await pul.queue.write_queue(
    system, "topic",
    backend="persisting",
    backend_options={"enable_wal": True},
)
# 所有未提交的数据现在可用
```

## Schema 演进

### 添加字段

```python
# 原始 Schema
await writer.put({"id": "1", "name": "Alice"})

# 添加新字段（带默认值）
await writer.put({"id": "2", "name": "Bob", "age": 30})

# 缺失字段获得默认值
```

### 字段类型变更

```python
# PersistingBackend 处理兼容的类型变更
# int → float 等
```

## 压缩

### 可用算法

| 算法 | 压缩比 | 速度 | 用途 |
|------|--------|------|------|
| `zstd` | 高 | 中等 | 通用 |
| `lz4` | 中等 | 快 | 低延迟 |
| `snappy` | 低 | 非常快 | 实时 |

### 示例

```python
import pulsing as pul

writer = await pul.queue.write_queue(
    system, "topic",
    backend="persisting",
    backend_options={"compression": "zstd"},
)
```

## 监控

### Prometheus 指标

当 `enable_metrics=True` 时，导出以下指标：

| 指标 | 类型 | 说明 |
|------|------|------|
| `persisting_records_total` | Counter | 写入的总记录数 |
| `persisting_buffer_size` | Gauge | 当前缓冲区大小 |
| `persisting_flush_duration_seconds` | Histogram | 刷新延迟 |
| `persisting_wal_size_bytes` | Gauge | WAL 文件大小 |

### 访问指标

```python
# 获取当前统计
stats = await writer.stats()
print(stats)
# {
#     'total_count': 1000,
#     'buffer_count': 50,
#     'persisted_count': 950,
#     'wal_size': 1024000,
#     'compression': 'zstd',
# }
```

## 最佳实践

### 生产配置

```python
import pulsing as pul

# 推荐的生产设置
writer = await pul.queue.write_queue(
    system,
    topic="production",
    backend="persisting",
    storage_path="/data/queues",
    batch_size=1000,
    backend_options={
        "enable_wal": True,
        "compression": "zstd",
        "enable_metrics": True,
        "wal_sync_interval": 0.1,  # 低延迟
    },
)
```

### 高吞吐量

```python
import pulsing as pul

# 用于高吞吐量工作负载
writer = await pul.queue.write_queue(
    system,
    topic="high_throughput",
    backend="persisting",
    batch_size=10000,
    backend_options={
        "enable_wal": False,  # 牺牲持久性换取速度
        "compression": "lz4",  # 快速压缩
    },
)
```

### 高持久性

```python
import pulsing as pul

# 用于关键数据
writer = await pul.queue.write_queue(
    system,
    topic="critical_data",
    backend="persisting",
    batch_size=100,
    backend_options={
        "enable_wal": True,
        "wal_sync_interval": 0.0,  # 每次写入都同步
        "compression": "zstd",
    },
)
```

## 与 Lance 后端比较

| 功能 | Lance | Persisting |
|------|-------|------------|
| 基本持久化 | ✅ | ✅ |
| 版本控制 | ✅ | ✅ |
| 向量搜索 | ✅ | ✅ |
| WAL | ❌ | ✅ |
| Schema 演进 | ❌ | ✅ |
| 压缩选项 | 有限 | 完整 |
| Prometheus 指标 | ❌ | ✅ |

## 下一步

- [自定义后端](custom.md) - 实现自己的后端
- [架构设计](../design/architecture.md) - 设计详情
- [API 参考](../api_reference.md) - 详细 API 文档

