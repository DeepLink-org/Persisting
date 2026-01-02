# Persisting Backend

The Persisting backend extends the Lance backend with enterprise-grade features for production deployments.

## Overview

`PersistingBackend` provides all Lance features plus:

- **Write-Ahead Log (WAL)** - Crash recovery and data durability
- **Schema Evolution** - Dynamic schema changes without downtime
- **Compression** - Multiple compression algorithms
- **Monitoring** - Prometheus metrics export

## Installation

```bash
pip install persisting[pulsing]
```

## Basic Usage

```python
import pulsing as pul
import persisting as pst

# Register the backend
pul.queue.register_backend("persisting", pst.queue.PersistingBackend)

# Create writer with enhanced features
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

# Write data
await writer.put({"id": "1", "value": 42})
await writer.flush()
```

## Configuration

### Backend Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable_wal` | bool | False | Enable Write-Ahead Log |
| `compression` | str | None | Compression: "zstd", "lz4", "snappy" |
| `enable_metrics` | bool | False | Enable Prometheus metrics |
| `wal_sync_interval` | float | 1.0 | WAL sync interval in seconds |
| `max_wal_size` | int | 100MB | Maximum WAL file size |

### Full Configuration Example

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

### Overview

WAL ensures data durability by writing changes to a log before applying them:

```
Write Request → WAL → Buffer → Lance Storage
                ↓
           (Crash Recovery)
```

### Enabling WAL

```python
import pulsing as pul

writer = await pul.queue.write_queue(
    system, "topic",
    backend="persisting",
    backend_options={"enable_wal": True},
)
```

### Recovery

On startup, the backend automatically recovers uncommitted data from WAL:

```python
import pulsing as pul

# After a crash, data is recovered automatically
writer = await pul.queue.write_queue(
    system, "topic",
    backend="persisting",
    backend_options={"enable_wal": True},
)
# All uncommitted data is now available
```

## Schema Evolution

### Adding Fields

```python
# Original schema
await writer.put({"id": "1", "name": "Alice"})

# Add new field (with default)
await writer.put({"id": "2", "name": "Bob", "age": 30})

# Missing fields get default values
```

### Field Type Changes

```python
# PersistingBackend handles compatible type changes
# int → float, etc.
```

## Compression

### Available Algorithms

| Algorithm | Ratio | Speed | Use Case |
|-----------|-------|-------|----------|
| `zstd` | High | Medium | General purpose |
| `lz4` | Medium | Fast | Low latency |
| `snappy` | Low | Very Fast | Real-time |

### Example

```python
import pulsing as pul

writer = await pul.queue.write_queue(
    system, "topic",
    backend="persisting",
    backend_options={"compression": "zstd"},
)
```

## Monitoring

### Prometheus Metrics

When `enable_metrics=True`, the following metrics are exported:

| Metric | Type | Description |
|--------|------|-------------|
| `persisting_records_total` | Counter | Total records written |
| `persisting_buffer_size` | Gauge | Current buffer size |
| `persisting_flush_duration_seconds` | Histogram | Flush latency |
| `persisting_wal_size_bytes` | Gauge | WAL file size |

### Accessing Metrics

```python
# Get current stats
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

## Best Practices

### Production Configuration

```python
import pulsing as pul

# Recommended production settings
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
        "wal_sync_interval": 0.1,  # Low latency
    },
)
```

### High Throughput

```python
import pulsing as pul

# For high throughput workloads
writer = await pul.queue.write_queue(
    system,
    topic="high_throughput",
    backend="persisting",
    batch_size=10000,
    backend_options={
        "enable_wal": False,  # Trade durability for speed
        "compression": "lz4",  # Fast compression
    },
)
```

### High Durability

```python
import pulsing as pul

# For critical data
writer = await pul.queue.write_queue(
    system,
    topic="critical_data",
    backend="persisting",
    batch_size=100,
    backend_options={
        "enable_wal": True,
        "wal_sync_interval": 0.0,  # Sync every write
        "compression": "zstd",
    },
)
```

## Comparison with Lance Backend

| Feature | Lance | Persisting |
|---------|-------|------------|
| Basic persistence | ✅ | ✅ |
| Version control | ✅ | ✅ |
| Vector search | ✅ | ✅ |
| WAL | ❌ | ✅ |
| Schema evolution | ❌ | ✅ |
| Compression options | Limited | Full |
| Prometheus metrics | ❌ | ✅ |

## Next Steps

- [Custom Backends](custom.md) - Implement your own backend
- [Architecture](../design/architecture.md) - Design details
- [API Reference](../api_reference.md) - Detailed API documentation

