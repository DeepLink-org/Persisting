# API Reference

This document provides a detailed API reference for Persisting.

## persisting.queue

### LanceBackend

High-performance storage backend using Lance columnar format.

```python
class LanceBackend:
    """Lance-based storage backend for Pulsing queues."""
    
    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        flush_threshold: int = 1000,
        **kwargs,
    ) -> None:
        """
        Initialize Lance backend.
        
        Parameters
        ----------
        bucket_id : int
            Unique identifier for this bucket.
        storage_path : str
            Path to store Lance dataset.
        batch_size : int, optional
            Batch size for operations (default: 100).
        flush_threshold : int, optional
            Number of records before auto-flush (default: 1000).
        **kwargs
            Additional options (ignored).
        """
```

#### Methods

##### put

```python
async def put(self, record: dict[str, Any]) -> None:
    """
    Store a single record.
    
    Parameters
    ----------
    record : dict[str, Any]
        Record to store.
    """
```

##### put_batch

```python
async def put_batch(self, records: list[dict[str, Any]]) -> None:
    """
    Store multiple records efficiently.
    
    Parameters
    ----------
    records : list[dict[str, Any]]
        Records to store.
    """
```

##### get

```python
async def get(
    self,
    offset: int = 0,
    limit: int | None = None,
) -> list[dict[str, Any]]:
    """
    Retrieve records from storage.
    
    Parameters
    ----------
    offset : int, optional
        Starting offset (default: 0).
    limit : int | None, optional
        Maximum records to return (default: None = all).
    
    Returns
    -------
    list[dict[str, Any]]
        Retrieved records.
    """
```

##### get_stream

```python
async def get_stream(
    self,
    offset: int = 0,
    limit: int | None = None,
    block: bool = False,
) -> AsyncIterator[dict[str, Any]]:
    """
    Stream records from storage.
    
    Parameters
    ----------
    offset : int, optional
        Starting offset (default: 0).
    limit : int | None, optional
        Maximum records to return (default: None = unlimited).
    block : bool, optional
        If True, wait for new records when caught up (default: False).
    
    Yields
    ------
    dict[str, Any]
        Records from storage.
    """
```

##### flush

```python
async def flush(self) -> None:
    """
    Persist buffered data to Lance storage.
    
    Writes all buffered records to the Lance dataset and
    clears the buffer.
    """
```

##### stats

```python
async def stats(self) -> dict[str, Any]:
    """
    Get storage statistics.
    
    Returns
    -------
    dict[str, Any]
        Statistics including:
        - bucket_id: Bucket identifier
        - total_count: Total record count
        - buffer_count: Records in buffer
        - persisted_count: Records in Lance
        - storage_path: Path to Lance dataset
    """
```

##### total_count

```python
def total_count(self) -> int:
    """
    Get total record count.
    
    Returns
    -------
    int
        Total number of records (buffer + persisted).
    """
```

---

### PersistingBackend

Enhanced storage backend with WAL, compression, and metrics.

```python
class PersistingBackend:
    """Enhanced storage backend with enterprise features."""
    
    def __init__(
        self,
        bucket_id: int,
        storage_path: str,
        batch_size: int = 100,
        enable_wal: bool = False,
        compression: str | None = None,
        enable_metrics: bool = False,
        wal_sync_interval: float = 1.0,
        max_wal_size: int = 100 * 1024 * 1024,
        **kwargs,
    ) -> None:
        """
        Initialize Persisting backend.
        
        Parameters
        ----------
        bucket_id : int
            Unique identifier for this bucket.
        storage_path : str
            Path to store data.
        batch_size : int, optional
            Batch size for operations (default: 100).
        enable_wal : bool, optional
            Enable Write-Ahead Log (default: False).
        compression : str | None, optional
            Compression algorithm: "zstd", "lz4", "snappy" (default: None).
        enable_metrics : bool, optional
            Enable Prometheus metrics (default: False).
        wal_sync_interval : float, optional
            WAL sync interval in seconds (default: 1.0).
        max_wal_size : int, optional
            Maximum WAL file size in bytes (default: 100MB).
        **kwargs
            Additional options (ignored).
        """
```

#### Methods

Inherits all methods from `LanceBackend` with enhanced behavior:

- `put()` - Writes to WAL first if enabled
- `flush()` - Truncates WAL after successful flush
- `stats()` - Includes WAL and compression info

---

## Usage Examples

### Basic Usage

```python
import pulsing as pul
import persisting as pst

# Register backend
pul.queue.register_backend("lance", pst.queue.LanceBackend)

# Create writer
writer = await pul.queue.write_queue(
    system,
    topic="example",
    backend="lance",
    storage_path="/data/queues",
)

# Write data
await writer.put({"id": "1", "value": 42})
await writer.flush()

# Read data
reader = await pul.queue.read_queue(system, "example")
records = await reader.get(limit=10)
```

### With WAL

```python
import pulsing as pul
import persisting as pst

pul.queue.register_backend("persisting", pst.queue.PersistingBackend)

writer = await pul.queue.write_queue(
    system,
    topic="durable",
    backend="persisting",
    backend_options={
        "enable_wal": True,
        "wal_sync_interval": 0.1,
    },
)
```

### Streaming

```python
import pulsing as pul

reader = await pul.queue.read_queue(system, "example")

# Non-blocking stream
async for record in reader.get_stream(limit=100):
    process(record)

# Blocking stream (waits for new data)
async for record in reader.get_stream(block=True):
    process(record)
```

---

## Exceptions

### StorageError

Base exception for storage operations.

```python
class StorageError(Exception):
    """Base exception for storage operations."""
    pass
```

### WALError

Exception for WAL-related errors.

```python
class WALError(StorageError):
    """Exception for WAL operations."""
    pass
```

---

## Type Definitions

### StorageBackend Protocol

```python
from typing import Protocol, AsyncIterator, Any

class StorageBackend(Protocol):
    """Protocol for pluggable storage backends."""
    
    async def put(self, record: dict[str, Any]) -> None: ...
    async def put_batch(self, records: list[dict[str, Any]]) -> None: ...
    async def get(self, offset: int = 0, limit: int | None = None) -> list[dict[str, Any]]: ...
    async def get_stream(
        self, offset: int = 0, limit: int | None = None, block: bool = False
    ) -> AsyncIterator[dict[str, Any]]: ...
    async def flush(self) -> None: ...
    async def stats(self) -> dict[str, Any]: ...
    def total_count(self) -> int: ...
```

