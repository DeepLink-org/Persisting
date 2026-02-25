# Quick Start

Get started with Persisting in 5 minutes.

## What You're Getting

Persisting provides persistent storage for parameters, KV Cache, and trajectories — with Lance as the storage engine and GPU/host/SSD tiering. Currently available: **streaming append** (append-only queue). Coming soon: **tensor memory API** for multi-dimensional access.

## Step 1: Install

```bash
pip install persisting[lance]
```

## Step 2: Tensor Memory (coming soon)

The primary API — tensor-style subscript access to tiered memory:

```python
import persisting
from persisting.core import Dimension

SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
HEAD    = Dimension("head", "int")
TIME    = Dimension("time", "int")

kv = persisting.open("kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME), order_dim=TIME)

arr = kv["s1", 0, 2, 0:512].tensor()
```

## Step 3: Streaming Append (available now)

Append-only queue on Lance storage engine — for trajectory collection and event streaming:

```python
import asyncio
from persisting import Queue

async def main():
    queue = Queue("my_topic", storage_path="./data")

    for i in range(10):
        await queue.put({"id": str(i), "value": i * 10})
    await queue.flush()

    records = await queue.get(limit=100)
    print(f"Read {len(records)} records")

asyncio.run(main())
```

With metrics:

```python
queue = Queue("my_topic", storage_path="./data", enable_metrics=True)
await queue.put({"id": "1", "value": 42})
await queue.flush()
stats = await queue.stats()
print(stats["metrics"])
```

## Next Steps

- [User Guide](guide/index.md) — Detailed guides
- [Design Docs](design/index.md) — Architecture and TTAS specification
- [API Reference](api_reference.md) — Full API documentation
