# Architecture

This document describes the overall architecture of Persisting.

## System Components

### 1. StorageBackend Protocol

The core abstraction that all backends implement:

```python
class StorageBackend(Protocol):
    """Storage backend protocol for pluggable persistence."""
    
    async def put(self, record: dict[str, Any]) -> None:
        """Store a single record."""
    
    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        """Store multiple records efficiently."""
    
    async def get(self, offset: int, limit: int | None) -> list[dict]:
        """Retrieve records from offset."""
    
    async def get_stream(
        self, offset: int, limit: int | None, block: bool
    ) -> AsyncIterator[dict]:
        """Stream records, optionally blocking for new data."""
    
    async def flush(self) -> None:
        """Persist buffered data to storage."""
    
    async def stats(self) -> dict[str, Any]:
        """Return storage statistics."""
    
    def total_count(self) -> int:
        """Return total record count."""
```

### 2. LanceBackend

High-performance storage using Lance columnar format:

```
┌─────────────────────────────────────────┐
│             LanceBackend                │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │         Memory Buffer           │   │
│  │  (Records waiting for flush)    │   │
│  └──────────────┬──────────────────┘   │
│                 │                       │
│                 ▼ flush()               │
│  ┌─────────────────────────────────┐   │
│  │        Lance Dataset            │   │
│  │  (Columnar storage on disk)     │   │
│  └─────────────────────────────────┘   │
│                                         │
└─────────────────────────────────────────┘
```

Features:
- In-memory buffering for write coalescing
- Automatic schema inference
- Version control built-in
- High-performance columnar storage

### 3. PersistingBackend

Enhanced backend with enterprise features:

```
┌─────────────────────────────────────────────────────────┐
│                   PersistingBackend                      │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │                  WAL Layer                       │   │
│  │  (Write-Ahead Log for durability)               │   │
│  └──────────────────────┬──────────────────────────┘   │
│                         │                               │
│                         ▼                               │
│  ┌─────────────────────────────────────────────────┐   │
│  │               Memory Buffer                      │   │
│  │  (With compression support)                     │   │
│  └──────────────────────┬──────────────────────────┘   │
│                         │                               │
│                         ▼ flush()                       │
│  ┌─────────────────────────────────────────────────┐   │
│  │              Lance Dataset                       │   │
│  │  (With schema evolution support)                │   │
│  └─────────────────────────────────────────────────┘   │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │            Metrics Collector                     │   │
│  │  (Prometheus metrics export)                    │   │
│  └─────────────────────────────────────────────────┘   │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

Features:
- All LanceBackend features
- Write-Ahead Log for crash recovery
- Schema evolution support
- Compression options
- Prometheus metrics

## Data Flow

### Write Path

```
Application
    │
    ▼ put(record)
┌─────────────────┐
│  QueueWriter    │
└────────┬────────┘
         │
         ▼ route by bucket_column
┌─────────────────┐
│ StorageManager  │
└────────┬────────┘
         │
         ▼ send to bucket owner
┌─────────────────┐
│ BucketStorage   │
└────────┬────────┘
         │
         ▼ delegate to backend
┌─────────────────┐
│ StorageBackend  │──► WAL (if enabled)
└────────┬────────┘
         │
         ▼ buffer
┌─────────────────┐
│  Memory Buffer  │
└────────┬────────┘
         │
         ▼ flush() when threshold reached
┌─────────────────┐
│  Lance Dataset  │
└─────────────────┘
```

### Read Path

```
Application
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
         ▼ query each bucket
┌─────────────────────────────────────┐
│         BucketStorage (parallel)    │
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
│         Merge Results              │
└─────────────────────────────────────┘
         │
         ▼
    Application
```

## Bucket Distribution

Records are distributed across buckets using consistent hashing:

```
┌──────────────────────────────────────────────────────────┐
│                    Bucket Ring                           │
│                                                          │
│                       Node A                             │
│                    ┌─────────┐                          │
│              ┌─────│ Bucket 0│─────┐                    │
│              │     └─────────┘     │                    │
│              │                     │                    │
│        ┌─────┴───┐           ┌────┴────┐               │
│   Node C│Bucket 3│           │Bucket 1 │Node B          │
│        └─────┬───┘           └────┬────┘               │
│              │                    │                     │
│              │     ┌─────────┐    │                     │
│              └─────│ Bucket 2│────┘                     │
│                    └─────────┘                          │
│                       Node B                             │
│                                                          │
└──────────────────────────────────────────────────────────┘

Record routing:
  hash(record[bucket_column]) % num_buckets → bucket_id
  bucket_id → owner_node (via consistent hashing)
```

## Concurrency Model

### Thread Safety

All backends use `asyncio.Condition` for safe concurrent access:

```python
class Backend:
    def __init__(self):
        self._condition = asyncio.Condition()
    
    async def put(self, record):
        async with self._condition:
            self._buffer.append(record)
            self._condition.notify_all()  # Wake readers
    
    async def get_stream(self, block=False):
        async with self._condition:
            while no_data_available:
                if not block:
                    return
                await self._condition.wait()  # Wait for data
```

### Blocking Reads

The `get_stream(block=True)` pattern enables consumer blocking:

```python
async for record in reader.get_stream(block=True):
    # Will wait for new records when caught up
    process(record)
```

## Configuration

### Backend Selection

```python
# By name (must be registered)
writer = await write_queue(system, topic, backend="lance")

# By class (auto-registers)
writer = await write_queue(system, topic, backend=LanceBackend)
```

### Backend Options

Options are passed through to the backend constructor:

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

## Error Handling

### Write Failures

```python
try:
    await writer.put(record)
except StorageError as e:
    # Handle storage failure
    logger.error(f"Write failed: {e}")
```

### Recovery

With WAL enabled, uncommitted data is recovered on startup:

```
Startup
    │
    ▼
Check for WAL files
    │
    ├─► No WAL: Normal startup
    │
    └─► WAL exists: Replay uncommitted records
            │
            ▼
        Apply to buffer
            │
            ▼
        Continue normal operation
```

## Future Considerations

1. **Multi-region replication** - Cross-datacenter data sync
2. **Tiered storage** - Hot/cold data management
3. **Query pushdown** - Filter evaluation in storage layer
4. **Backup/restore** - Point-in-time recovery

