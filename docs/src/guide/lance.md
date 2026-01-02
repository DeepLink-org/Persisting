# Lance Backend

The Lance backend provides high-performance persistent storage using the Lance columnar data format.

## Overview

Lance is a modern columnar data format optimized for ML workflows. The `LanceBackend` brings these capabilities to Pulsing queues:

- **High-performance random access** - Efficient retrieval of any record
- **Columnar storage** - Optimal for analytical queries
- **Version control** - Built-in data versioning
- **Vector search** - Native support for similarity search

## Installation

```bash
pip install persisting[pulsing]
```

## Basic Usage

```python
from pulsing.queue import register_backend, write_queue, read_queue
from persisting.queue import LanceBackend

# Register the backend
register_backend("lance", LanceBackend)

# Create writer
writer = await write_queue(
    system,
    topic="my_topic",
    backend="lance",
    storage_path="/data/queues",
)

# Write data
await writer.put({"id": "1", "text": "Hello", "vector": [0.1, 0.2, 0.3]})
await writer.put({"id": "2", "text": "World", "vector": [0.4, 0.5, 0.6]})

# Flush to persist
await writer.flush()

# Read data
reader = await read_queue(system, "my_topic")
records = await reader.get(limit=10)
```

## Configuration

### Backend Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `flush_threshold` | int | 1000 | Records before auto-flush |

### Example Configuration

```python
writer = await write_queue(
    system,
    topic="configured_topic",
    backend="lance",
    storage_path="/data/queues",
    batch_size=100,
    backend_options={
        "flush_threshold": 500,
    },
)
```

## Data Storage

### Directory Structure

```
/data/queues/
└── my_topic/
    └── bucket_0/
        ├── data.lance/
        │   ├── _versions/
        │   │   ├── 1.manifest
        │   │   └── 2.manifest
        │   └── data/
        │       ├── 0.lance
        │       └── 1.lance
        └── ...
```

### Schema

The Lance backend automatically infers schema from the first record:

```python
# First record defines schema
await writer.put({
    "id": "1",
    "text": "Hello",
    "value": 42,
    "vector": [0.1, 0.2, 0.3],
})

# Subsequent records must match schema
await writer.put({
    "id": "2",
    "text": "World",
    "value": 100,
    "vector": [0.4, 0.5, 0.6],
})
```

## Performance

### Buffering

The Lance backend buffers records in memory before persisting:

```python
# Records are buffered
for i in range(100):
    await writer.put({"id": str(i), "value": i})

# Explicit flush persists to disk
await writer.flush()
```

### Auto-flush

When `flush_threshold` is reached, data is automatically persisted:

```python
writer = await write_queue(
    system, "topic",
    backend="lance",
    backend_options={"flush_threshold": 500},
)

# After 500 records, auto-flush occurs
for i in range(600):
    await writer.put({"id": str(i), "value": i})
```

## Streaming

The Lance backend supports efficient streaming:

```python
reader = await read_queue(system, "my_topic")

# Non-blocking stream
async for record in reader.get_stream(offset=0, limit=100):
    process(record)

# Blocking stream (waits for new data)
async for record in reader.get_stream(block=True):
    process(record)
```

## Statistics

Get storage statistics:

```python
stats = await writer.stats()
print(f"Total records: {stats['total_count']}")
print(f"Buffered: {stats['buffer_count']}")
print(f"Persisted: {stats['persisted_count']}")
```

## Best Practices

1. **Choose appropriate batch size** - Larger batches improve write throughput
2. **Flush regularly** - Call `flush()` to ensure data durability
3. **Use appropriate storage path** - Use fast SSD storage for production
4. **Monitor buffer size** - Avoid memory pressure with large buffers

## Next Steps

- [Persisting Backend](persisting.md) - Enhanced features with WAL
- [Custom Backends](custom.md) - Implement your own backend
- [API Reference](../api_reference.md) - Detailed API documentation

