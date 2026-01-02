# Custom Backends

This guide shows how to implement custom storage backends for Pulsing queues.

## StorageBackend Protocol

All backends must implement the `StorageBackend` protocol:

```python
from typing import Protocol, AsyncIterator, Any

class StorageBackend(Protocol):
    """Protocol for pluggable storage backends."""
    
    async def put(self, record: dict[str, Any]) -> None:
        """Store a single record."""
        ...
    
    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        """Store multiple records efficiently."""
        ...
    
    async def get(
        self,
        offset: int = 0,
        limit: int | None = None
    ) -> list[dict[str, Any]]:
        """Retrieve records from storage."""
        ...
    
    async def get_stream(
        self,
        offset: int = 0,
        limit: int | None = None,
        block: bool = False
    ) -> AsyncIterator[dict[str, Any]]:
        """Stream records from storage."""
        ...
    
    async def flush(self) -> None:
        """Persist any buffered data."""
        ...
    
    async def stats(self) -> dict[str, Any]:
        """Return storage statistics."""
        ...
    
    def total_count(self) -> int:
        """Return total record count."""
        ...
```

## Basic Implementation

Here's a minimal backend implementation:

```python
import asyncio
from typing import Any, AsyncIterator

class SimpleBackend:
    """A simple in-memory backend."""
    
    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        **kwargs,
    ):
        self.bucket_id = bucket_id
        self.storage_path = storage_path
        self.batch_size = batch_size
        self._records: list[dict[str, Any]] = []
        self._condition = asyncio.Condition()
    
    async def put(self, record: dict[str, Any]) -> None:
        """Store a single record."""
        async with self._condition:
            self._records.append(record)
            self._condition.notify_all()
    
    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        """Store multiple records."""
        async with self._condition:
            self._records.extend(records)
            self._condition.notify_all()
    
    async def get(
        self,
        offset: int = 0,
        limit: int | None = None
    ) -> list[dict[str, Any]]:
        """Retrieve records."""
        if limit is None:
            return self._records[offset:]
        return self._records[offset:offset + limit]
    
    async def get_stream(
        self,
        offset: int = 0,
        limit: int | None = None,
        block: bool = False
    ) -> AsyncIterator[dict[str, Any]]:
        """Stream records."""
        current = offset
        count = 0
        
        while True:
            async with self._condition:
                while current >= len(self._records):
                    if not block:
                        return
                    await self._condition.wait()
                
                while current < len(self._records):
                    if limit is not None and count >= limit:
                        return
                    yield self._records[current]
                    current += 1
                    count += 1
    
    async def flush(self) -> None:
        """No-op for in-memory storage."""
        pass
    
    async def stats(self) -> dict[str, Any]:
        """Return statistics."""
        return {
            "bucket_id": self.bucket_id,
            "total_count": len(self._records),
            "buffer_count": 0,
            "persisted_count": len(self._records),
        }
    
    def total_count(self) -> int:
        """Return total record count."""
        return len(self._records)
```

## Registering Custom Backends

```python
import pulsing as pul

# Register your backend
pul.queue.register_backend("simple", SimpleBackend)

# Use it
writer = await pul.queue.write_queue(
    system,
    topic="my_topic",
    backend="simple",
)
```

## Advanced Implementation

### With Persistence

```python
import json
from pathlib import Path

class FileBackend:
    """A file-based backend with persistence."""
    
    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        **kwargs,
    ):
        self.bucket_id = bucket_id
        self.storage_path = Path(storage_path)
        self.batch_size = batch_size
        self._buffer: list[dict[str, Any]] = []
        self._persisted_count = 0
        self._condition = asyncio.Condition()
        
        # Load existing data
        self._load()
    
    def _load(self) -> None:
        """Load persisted data."""
        self.storage_path.mkdir(parents=True, exist_ok=True)
        data_file = self.storage_path / "data.json"
        if data_file.exists():
            with open(data_file) as f:
                data = json.load(f)
                self._persisted_count = len(data)
    
    async def put(self, record: dict[str, Any]) -> None:
        async with self._condition:
            self._buffer.append(record)
            if len(self._buffer) >= self.batch_size:
                await self._flush_internal()
            self._condition.notify_all()
    
    async def _flush_internal(self) -> None:
        """Internal flush without lock."""
        if not self._buffer:
            return
        
        data_file = self.storage_path / "data.json"
        
        # Load existing
        existing = []
        if data_file.exists():
            with open(data_file) as f:
                existing = json.load(f)
        
        # Append new
        existing.extend(self._buffer)
        
        # Write back
        with open(data_file, "w") as f:
            json.dump(existing, f)
        
        self._persisted_count += len(self._buffer)
        self._buffer.clear()
    
    async def flush(self) -> None:
        """Persist buffered data."""
        async with self._condition:
            await self._flush_internal()
```

### With Blocking Support

The `get_stream` method with `block=True` should wait for new data:

```python
async def get_stream(
    self,
    offset: int = 0,
    limit: int | None = None,
    block: bool = False
) -> AsyncIterator[dict[str, Any]]:
    """Stream records with blocking support."""
    current = offset
    count = 0
    
    while True:
        async with self._condition:
            # Wait for data if blocking
            while current >= self.total_count():
                if not block:
                    return
                # Wait with timeout to check periodically
                try:
                    await asyncio.wait_for(
                        self._condition.wait(),
                        timeout=1.0
                    )
                except asyncio.TimeoutError:
                    continue
            
            # Get available records
            records = await self.get(current, limit)
            
        for record in records:
            if limit is not None and count >= limit:
                return
            yield record
            current += 1
            count += 1
```

## Testing Custom Backends

```python
import pytest
import pulsing as pul

@pytest.fixture
def custom_backend():
    pul.queue.register_backend("custom", CustomBackend)
    yield
    # Cleanup if needed

@pytest.mark.asyncio
async def test_custom_backend_write_read(actor_system, custom_backend):
    # Write
    writer = await pul.queue.write_queue(
        actor_system,
        topic="test",
        backend="custom",
        storage_path="/tmp/test",
    )
    
    await writer.put({"id": "1", "value": 42})
    await writer.flush()
    
    # Read
    reader = await pul.queue.read_queue(actor_system, "test")
    records = await reader.get(limit=10)
    
    assert len(records) == 1
    assert records[0]["id"] == "1"
```

## Best Practices

1. **Thread Safety**: Use `asyncio.Condition` for safe concurrent access
2. **Buffering**: Buffer writes for better performance
3. **Error Handling**: Handle storage errors gracefully
4. **Statistics**: Provide useful stats for monitoring
5. **Testing**: Write comprehensive tests

## Next Steps

- [Architecture](../design/architecture.md) - Design details
- [API Reference](../api_reference.md) - Detailed API documentation

