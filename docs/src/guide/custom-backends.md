# Custom Backends

How to implement a custom storage backend. Custom backends implement the `StorageBackend` interface and can be used internally by the queue system.

## StorageBackend Protocol

Implement these methods (defined in `persisting.queue.backend`):

```python
from typing import Any, AsyncIterator

class MyBackend:
    def __init__(self, bucket_id: int, storage_path: str, batch_size: int = 100, **kwargs):
        ...

    async def put(self, record: dict[str, Any]) -> None: ...
    async def put_batch(self, records: list[dict[str, Any]]) -> None: ...
    async def get(self, limit: int, offset: int) -> list[dict[str, Any]]: ...
    async def get_stream(self, limit: int, offset: int, wait: bool = False, timeout: float | None = None) -> AsyncIterator[list[dict[str, Any]]]: ...
    async def flush(self) -> None: ...
    async def stats(self) -> dict[str, Any]: ...
    def total_count(self) -> int: ...
```

The constructor must accept `bucket_id`, `storage_path`, and `batch_size` as positional/keyword arguments, plus `**kwargs` for forward compatibility.

## Minimal Example

```python
import asyncio
from typing import Any, AsyncIterator

class SimpleBackend:
    def __init__(self, bucket_id: int, storage_path: str, batch_size: int = 100, **kwargs):
        self.bucket_id = bucket_id
        self._records: list[dict[str, Any]] = []
        self._condition = asyncio.Condition()

    def total_count(self) -> int:
        return len(self._records)

    async def put(self, record: dict[str, Any]) -> None:
        async with self._condition:
            self._records.append(record)
            self._condition.notify_all()

    async def put_batch(self, records: list[dict[str, Any]]) -> None:
        async with self._condition:
            self._records.extend(records)
            self._condition.notify_all()

    async def get(self, limit: int, offset: int) -> list[dict[str, Any]]:
        return self._records[offset : offset + limit]

    async def get_stream(
        self, limit: int, offset: int, wait: bool = False, timeout: float | None = None,
    ) -> AsyncIterator[list[dict[str, Any]]]:
        pos = offset
        remaining = limit
        while remaining > 0:
            async with self._condition:
                if pos >= len(self._records):
                    if not wait:
                        return
                    try:
                        coro = self._condition.wait()
                        if timeout:
                            await asyncio.wait_for(coro, timeout=timeout)
                        else:
                            await coro
                        continue
                    except asyncio.TimeoutError:
                        return
                batch = self._records[pos : pos + min(remaining, 100)]
            if batch:
                yield batch
                pos += len(batch)
                remaining -= len(batch)
            elif not wait:
                break

    async def flush(self) -> None:
        pass  # No-op for in-memory storage

    async def stats(self) -> dict[str, Any]:
        return {"bucket_id": self.bucket_id, "total_count": len(self._records)}
```

Custom backends that implement this interface can be used internally where the queue system accepts a backend instance. The public API (`Queue`) uses Lance-based storage; custom backends are for advanced use cases and framework integration.

## Tips

1. **Use `asyncio.Condition`** for safe concurrent access and blocking reads.
2. **Accept `**kwargs`** in `__init__` for forward compatibility with new options.
3. **Call `notify_all()`** after writes so `get_stream(wait=True)` readers wake up.
4. **Return batches** from `get_stream`, not individual records.

## Next Steps

- [Architecture](../design/architecture.md) — How backends fit into the system
- [API Reference](../api_reference.md) — Full API documentation
