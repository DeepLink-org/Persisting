# Queue Capabilities

Overview of `persisting.Queue` — the unified API for append-only persistent queues.

## Queue API

```python
from persisting import Queue

queue = Queue("my_topic", storage_path="./data")
await queue.put({"id": "1", "value": 42})
await queue.flush()
records = await queue.get(limit=100)
```

## With Metrics

For production observability, enable operation counters:

```python
queue = Queue("my_topic", storage_path="./data", enable_metrics=True)
await queue.put({"id": "1", "value": 42})
await queue.flush()
stats = await queue.stats()
```

## Queue Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `storage_path` | `str` | `"./data"` | Root directory for queue data |
| `batch_size` | `int` | `100` | Auto-flush when buffer reaches this size |
| `enable_metrics` | `bool` | `False` | Collect operation counters (put/get/flush counts) |

## Comparison

| Feature | Queue (default) | Queue (enable_metrics=True) |
|---------|-----------------|-----------------------------|
| Persistence | Yes | Yes |
| Recovery on restart | Yes | Yes |
| Auto-flush | Yes | Yes |
| Operation metrics | No | Yes |

## Implementation

`Queue` uses Lance-based persistent storage internally:

- **Columnar storage** — Efficient on-disk format
- **Memory buffering** — Write coalescing for throughput
- **Auto-flush** — Persist when buffer reaches `batch_size`

When `enable_metrics=True`, it uses an extended backend that tracks `put_count`, `get_count`, `flush_count`, and `last_flush_time`.

## Next Steps

- [Lance Backend](lance.md) — What Queue does internally (buffering, flush, storage)
- [Custom Backends](custom.md) — Implement your own backend for internal use
