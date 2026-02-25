# API Reference

Persisting extends Pulsing with distributed tiered memory. This page documents the currently available APIs. The tensor memory API (`persisting.open`, `Handler`, tiered access) is under development — see the [design docs](design/index.md) for the specification.

## Queue

High-level persistent queue backed by Lance storage. Use this for all queue operations.

### Constructor

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

- `name`: Queue name (topic) — used as subdirectory under `storage_path`.
- `storage_path`: Root directory for queue data.
- `batch_size`: Auto-flush when buffer reaches this size.
- `auto_flush_interval_sec`: If > 0, flush at this interval (seconds).
- `enable_metrics`: Collect operation counters (put/get/flush).

### Methods

```python
# Write
await queue.put({"id": "1", "value": 42})
await queue.put_batch([{"id": "2", "value": 100}, ...])
await queue.flush()

# Read
records = await queue.get(limit=100, offset=0)

# Stats
stats = await queue.stats()
len(queue)  # total_count
queue.close()
```

### Example

```python
from persisting import Queue

queue = Queue("my_topic", storage_path="./data")
await queue.put({"id": "1", "value": 42})
await queue.flush()
records = await queue.get(limit=100)
```

---

## Internal Backends (persisting.queue)

These backends power `persisting.Queue`. Most users should use `Queue` directly.

### LanceBackend

Lance-based persistent storage backend for Pulsing streaming queues.

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

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `bucket_id` | `int` | — | Bucket identifier (assigned by Pulsing) |
| `storage_path` | `str` | — | Directory for Lance dataset files |
| `batch_size` | `int` | `100` | Auto-flush when buffer reaches this size |

**Methods:**

#### put

```python
async def put(self, record: dict[str, Any]) -> None
```

Store a single record. Auto-flushes when buffer reaches `batch_size`.

#### put_batch

```python
async def put_batch(self, records: list[dict[str, Any]]) -> None
```

Store multiple records at once.

#### get

```python
async def get(self, limit: int, offset: int) -> list[dict[str, Any]]
```

Read records from storage (persisted + buffered).

#### get_stream

```python
async def get_stream(
    self,
    limit: int,
    offset: int,
    wait: bool = False,
    timeout: float | None = None,
) -> AsyncIterator[list[dict[str, Any]]]
```

Stream records in batches. When `wait=True`, blocks until new data arrives or `timeout` is reached.

#### flush

```python
async def flush(self) -> None
```

Persist all buffered records to the Lance dataset on disk.

#### stats

```python
async def stats(self) -> dict[str, Any]
```

Returns storage statistics:

```python
{
    "bucket_id": 0,
    "backend": "lance",
    "storage_path": "./data",
    "buffer_size": 42,
    "persisted_count": 1000,
    "total_count": 1042,
}
```

#### total_count

```python
def total_count(self) -> int
```

Returns total record count (persisted + buffered).

---

### PersistingBackend

Extends `LanceBackend` with operation metrics.

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

**Additional Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `enable_metrics` | `bool` | `True` | Collect operation counters |

**Additional Methods:**

#### get_metrics

```python
def get_metrics(self) -> dict[str, int | float]
```

Returns a snapshot of collected metrics:

```python
{
    "put_count": 42,
    "get_count": 10,
    "flush_count": 3,
    "last_flush_time": 1708012345.0,
}
```

The `stats()` method also includes a `"metrics"` key when metrics are enabled.

---

## StorageBackend Protocol

Defined in `pulsing.streaming.backend`. Any class implementing these methods can be used as a storage backend:

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

Custom backends implementing this interface can be passed to `Queue` internals or used directly where needed.
