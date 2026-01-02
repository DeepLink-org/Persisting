# Queue Backends

This guide provides an overview of the storage backends available in Persisting.

## Backend Protocol

All backends implement the `StorageBackend` protocol:

```python
from typing import Protocol, AsyncIterator, Any

class StorageBackend(Protocol):
    """Protocol for pluggable storage backends."""
    
    async def put(self, record: dict[str, Any]) -> None:
        """Store a single record."""
        ...
    
    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        """Store multiple records."""
        ...
    
    async def get(self, offset: int = 0, limit: int | None = None) -> list[dict[str, Any]]:
        """Retrieve records."""
        ...
    
    async def get_stream(
        self,
        offset: int = 0,
        limit: int | None = None,
        block: bool = False
    ) -> AsyncIterator[dict[str, Any]]:
        """Stream records."""
        ...
    
    async def flush(self) -> None:
        """Persist buffered data."""
        ...
    
    async def stats(self) -> dict[str, Any]:
        """Get storage statistics."""
        ...
    
    def total_count(self) -> int:
        """Get total record count."""
        ...
```

## Available Backends

### MemoryBackend (Pulsing)

Simple in-memory storage without persistence:

```python
import pulsing as pul

writer = await pul.queue.write_queue(system, "topic", backend="memory")
```

**Characteristics:**

- ✅ Fast read/write
- ✅ No dependencies
- ❌ No persistence
- ❌ Data lost on restart

**Use cases:** Testing, development, temporary data

### LanceBackend (Persisting)

Lance-based persistent storage:

```python
import pulsing as pul
import persisting as pst

pul.queue.register_backend("lance", pst.queue.LanceBackend)
writer = await pul.queue.write_queue(system, "topic", backend="lance", storage_path="/data")
```

**Characteristics:**

- ✅ High-performance columnar storage
- ✅ Data persistence
- ✅ Version control
- ✅ Vector search support

**Use cases:** Production workloads, ML data storage

### PersistingBackend (Persisting)

Enhanced backend with advanced features:

```python
import pulsing as pul
import persisting as pst

pul.queue.register_backend("persisting", pst.queue.PersistingBackend)
writer = await pul.queue.write_queue(
    system, "topic",
    backend="persisting",
    storage_path="/data",
    backend_options={"enable_wal": True}
)
```

**Characteristics:**

- ✅ All Lance features
- ✅ Write-Ahead Log (WAL)
- ✅ Schema evolution
- ✅ Prometheus metrics

**Use cases:** Production with durability requirements

## Backend Comparison

| Feature | Memory | Lance | Persisting |
|---------|--------|-------|------------|
| Persistence | ❌ | ✅ | ✅ |
| WAL | ❌ | ❌ | ✅ |
| Compression | ❌ | ✅ | ✅ |
| Metrics | ❌ | ❌ | ✅ |
| Schema Evolution | N/A | ❌ | ✅ |
| Vector Search | ❌ | ✅ | ✅ |

## Backend Registration

Register custom backends with Pulsing:

```python
import pulsing as pul

# Register a backend
pul.queue.register_backend("my_backend", MyBackendClass)

# Get a registered backend
backend_class = pul.queue.get_backend_class("my_backend")

# List all backends
available = pul.queue.list_backends()
print(available)  # ['memory', 'my_backend', ...]
```

## Next Steps

- [Lance Backend](lance.md) - Detailed Lance backend guide
- [Persisting Backend](persisting.md) - Enhanced backend features
- [Custom Backends](custom.md) - Implement your own backend

