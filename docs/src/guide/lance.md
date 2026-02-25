# Lance Backend

`Queue` uses Lance-based persistent storage internally. This page explains how it works.

## Overview

[Lance](https://github.com/lancedb/lance) is a columnar data format optimized for ML workflows. `Queue` builds on Lance for persistence:

- **Columnar storage** — Efficient on-disk format
- **Memory buffering** — Write coalescing for throughput
- **Auto-flush** — Persist when buffer reaches `batch_size`
- **Recovery** — Persisted record count recovered on restart

## Installation

```bash
pip install persisting[lance]
```

## Usage

```python
from persisting import Queue

queue = Queue("my_topic", storage_path="./data/queues")

# Write
await queue.put({"id": "1", "text": "Hello", "value": 42})
await queue.put({"id": "2", "text": "World", "value": 100})
await queue.flush()

# Read
records = await queue.get(limit=10)
```

## Configuration

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `batch_size` | `int` | `100` | Auto-flush when buffer reaches this size |
| `storage_path` | `str` | — | Directory for queue data (queue name becomes subdirectory) |

```python
queue = Queue(
    "topic",
    storage_path="./data",
    batch_size=500,
)
```

## Data Storage

Records are stored as a Lance dataset at `{storage_path}/{name}/data.lance`:

```
./data/queues/
└── my_topic/
    └── data.lance/
        ├── _versions/
        └── data/
```

Schema is inferred from the first batch of records. Supported types:

| Python type | Arrow type |
|-------------|------------|
| `int` | `int64` |
| `float` | `float64` |
| `bool` | `bool` |
| Other | `string` |

## Buffering and Flush

Records accumulate in a memory buffer. Flush happens when:

1. Buffer reaches `batch_size` (auto-flush)
2. `flush()` is called explicitly

```python
queue = Queue("topic", storage_path="./data")

# Records are buffered
for i in range(100):
    await queue.put({"id": str(i), "value": i})

# Explicit flush ensures persistence
await queue.flush()
```

## Statistics

```python
stats = await queue.stats()
# {
#     "bucket_id": 0,
#     "backend": "lance",
#     "storage_path": "./data/my_topic",
#     "buffer_size": 42,
#     "persisted_count": 1000,
#     "total_count": 1042,
# }
```

## Next Steps

- [Custom Backends](custom.md) — Implement your own backend
- [API Reference](../api_reference.md) — Full API documentation
