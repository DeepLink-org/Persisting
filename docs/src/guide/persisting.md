# Persisting Queue with Metrics

Enable `enable_metrics=True` for operation metrics and production observability.

## Usage

```python
from persisting import Queue

queue = Queue("my_topic", storage_path="./data", enable_metrics=True)

await queue.put({"id": "1", "value": 42})
await queue.flush()

stats = await queue.stats()
```

## Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable_metrics` | `bool` | `False` | Collect operation counters |

All other `Queue` parameters (`batch_size`, `storage_path`) also apply.

## Metrics

When `enable_metrics=True`, the following counters are tracked:

| Metric | Type | Description |
|--------|------|-------------|
| `put_count` | int | Total records written |
| `get_count` | int | Total get() calls |
| `flush_count` | int | Total flush() calls |
| `last_flush_time` | float | Timestamp of last flush |

### Access Metrics

```python
stats = await queue.stats()
print(stats["metrics"])
# {"put_count": 42, "get_count": 10, "flush_count": 3, "last_flush_time": 1708012345.0}
```

## Comparison

| Feature | Queue (default) | Queue (enable_metrics=True) |
|---------|-----------------|-----------------------------|
| Persistence | Yes | Yes |
| Memory buffering | Yes | Yes |
| Auto-flush | Yes | Yes |
| Recovery | Yes | Yes |
| Operation metrics | No | Yes |

## Future

Planned additions:

- Write-Ahead Log (WAL) for crash recovery
- Compaction support
- Prometheus metrics export

## Next Steps

- [Lance Backend](lance.md) — Base storage details
- [Custom Backends](custom.md) — Implement your own
- [API Reference](../api_reference.md) — Full API documentation
