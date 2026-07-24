# Queue API

## `Queue`

High-level persistent queue backed by Lance storage.

```python
from persisting import Queue

q = Queue(
    name: str,
    storage_path: str = "./data",
    *,
    batch_size: int = 100,
    auto_flush_interval_sec: float = 0.0,
    enable_metrics: bool = False,
    num_buckets: int = 4,
    bucket_column: str = "id",
    zerocopy_mode: str = "auto",
)
```

| Parameter | Description |
|-----------|-------------|
| `name` | Topic name — used as subdirectory under `storage_path` |
| `storage_path` | Root directory for queue data |
| `batch_size` | Auto-flush when buffer reaches this size |
| `auto_flush_interval_sec` | Periodic flush (0 = disabled) |
| `enable_metrics` | Collect put/get/flush counters |
| `num_buckets` | Hash bucket count (distributed mode) |
| `bucket_column` | Column for consistent hashing |
| `zerocopy_mode` | `"auto"`, `"force"`, or `"off"` |

### Write Methods

```python
await q.put(record: dict) → BatchMeta | None
await q.put(data: Any, partition_id: str = "default") → BatchMeta | None
await q.put_batch(records: list[dict], partition_id: str = "default") → None
await q.flush() → None
```

`put()` accepts either plain dicts (local mode) or tensor data via `partition_id` (distributed).

### Read Methods

```python
await q.get(limit: int = 100, offset: int = 0) → list[dict]
await q.get_meta(fields, batch_size, task_name, partition_id, sampler) → BatchMeta
await q.get_data(batch_meta: BatchMeta, partition_id) → Any
await q.get_batch(fields, batch_size, task_name, partition_id, sampler) → Any
await q.stream(limit, offset, wait=False, timeout=None) → AsyncIterator[list[dict]]
```

### Consumption Tracking

```python
await q.mark_consumed(task_name: str, global_indexes: list[int], partition_id: str = "default") → None
await q.reset_consumption(task_name: str, partition_id: str = "default") → None
await q.clear(global_indexes: list[int], partition_id: str = "default") → None
```

### Stats

```python
await q.stats() → dict
len(q) → int
q.close() → None
```

---

## `KVInterface`

Key-value access layer on top of Queue.

```python
from persisting import Queue, KVInterface

q = Queue("kv_store", storage_path="./data")
kv = KVInterface(q)
```

| Method | Description |
|--------|-------------|
| `await kv.kv_put(key, data, partition_id, tag=None)` | Put by key |
| `await kv.kv_batch_put(keys, data, partition_id, tags=None)` | Batch put |
| `await kv.kv_batch_get(keys, partition_id, fields=None)` | Get by keys |
| `await kv.kv_list(partition_id)` | List key→tag pairs |
| `await kv.kv_clear(keys, partition_id)` | Remove keys |

---

## Samplers

```python
from persisting import SequentialSampler, RankAwareSampler, GRPOGroupNSampler
```

| Sampler | Description |
|---------|-------------|
| `SequentialSampler()` | Simple sequential sampling |
| `RankAwareSampler()` | Each rank gets independent slices |
| `GRPOGroupNSampler(n_samples_per_prompt)` | N samples per prompt group |

---

## Internal Backends

These are used internally by `Queue`. Most users should not use them directly.

### `LanceBackend`

```python
from persisting.queue import LanceBackend

backend = LanceBackend(bucket_id, storage_path, batch_size=100, **kwargs)
await backend.put(record)
await backend.get(limit, offset)
await backend.flush()
await backend.stats()
```

### `PersistingBackend`

Extends `LanceBackend` with metrics:

```python
from persisting.queue import PersistingBackend

backend = PersistingBackend(bucket_id, storage_path, batch_size=100, enable_metrics=True)
await backend.stats()           # includes "metrics" key
await backend.get_metrics()     # metrics snapshot
```
