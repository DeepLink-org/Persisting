# Quick Start

This guide will help you get started with Persisting in minutes.

## Basic Usage

### Step 1: Install Persisting

```bash
pip install persisting[pulsing]
```

### Step 2: Register Backend with Pulsing

```python
import pulsing as pul
import persisting as pst

# Register the Lance backend
pul.queue.register_backend("lance", pst.queue.LanceBackend)
```

### Step 3: Create a Persistent Queue

```python
import asyncio
import pulsing as pul
import persisting as pst

async def main():
    # Register backend
    pul.queue.register_backend("lance", pst.queue.LanceBackend)
    
    # Create actor system
    system = await pul.actor.create_actor_system(pul.actor.SystemConfig.standalone())
    
    # Create queue writer with Lance persistence
    writer = await pul.queue.write_queue(
        system,
        topic="my_data",
        backend="lance",
        storage_path="/data/queues",
        num_buckets=4,
        batch_size=100,
    )
    
    # Write data
    for i in range(10):
        await writer.put({"id": str(i), "value": i * 10})
    
    # Flush to persist
    await writer.flush()
    
    # Create queue reader
    reader = await pul.queue.read_queue(system, "my_data")
    
    # Read data
    async for record in reader.get_stream():
        print(f"Read: {record}")
    
    await system.shutdown()

asyncio.run(main())
```

## Using Different Backends

### Memory Backend (for testing)

```python
import pulsing as pul

# Memory backend is built into Pulsing (no persistence)
writer = await pul.queue.write_queue(
    system,
    topic="test_topic",
    backend="memory",
)
```

### Lance Backend (recommended for production)

```python
import pulsing as pul
import persisting as pst

pul.queue.register_backend("lance", pst.queue.LanceBackend)

writer = await pul.queue.write_queue(
    system,
    topic="prod_topic",
    backend="lance",
    storage_path="/data/queues",
)
```

### Persisting Backend (enhanced features)

```python
import pulsing as pul
import persisting as pst

pul.queue.register_backend("persisting", pst.queue.PersistingBackend)

writer = await pul.queue.write_queue(
    system,
    topic="advanced_topic",
    backend="persisting",
    storage_path="/data/queues",
    backend_options={
        "enable_wal": True,
        "compression": "zstd",
    },
)
```

## Backend Options

### Lance Backend Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `flush_threshold` | int | 1000 | Records before auto-flush |

### Persisting Backend Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable_wal` | bool | False | Enable Write-Ahead Log |
| `compression` | str | None | Compression algorithm |
| `enable_metrics` | bool | False | Enable Prometheus metrics |

## Next Steps

- [User Guide](guide/index.md) - Learn more about storage backends
- [Lance Backend](guide/lance.md) - Lance backend details
- [Persisting Backend](guide/persisting.md) - Enhanced backend features
- [API Reference](api_reference.md) - Detailed API documentation

