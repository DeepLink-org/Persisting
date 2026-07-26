# Quick Start

Get started with Persisting in 5 minutes.

## Installation

```bash
pip install persisting[lance]
```

For CLI tools (`persisting traj`, `persisting ppilot`, `persisting search`), [build from source](installation.md#from-source).

---

## Core: Unified Tensor Storage

Persisting stores trajectories, parameters, and KV cache through the same `persisting.open()` interface. All three use TTAS (Tiered Tensor Address Space) — the same addressing, same Lance engine, same tiering.

```python
import persisting
from persisting.core import Dimension
```

### Parameters

Address model weights by name and shard:

```python
PARAM_ID = Dimension("param_id", "str")
SHARD    = Dimension("shard", "int")

ps = persisting.open("params/llama-70b",
    dims=(PARAM_ID, SHARD),
    backend="tiered",
    shape=(100, 8),
)

weights = ps["embed.weight", 0].tensor()
ps["lm_head.weight", 0].put(updated_tensor)
```

### KV Cache

Cross-session, multi-layer KV cache with block-tiered storage:

```python
SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
HEAD    = Dimension("head", "int")
TIME    = Dimension("time", "int")

kv = persisting.open("kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME),
    order_dim=TIME,
    backend="tiered",
    shape=(100, 32, 8, 4096),
    block_tokens=64,
)

h = kv["s1", 0, 2, 0:512]    # slice (zero copy)
arr = h.tensor()               # materialize from fastest tier
h.put(other_data)              # write back
kv.prefetch(("s1", 0, 0:1024))  # async pull blocks to host memory
```

### Trajectories (via Queue)

Trajectory events are stored through the Queue API, backed by the same Lance engine:

```python
from persisting import Queue

q = Queue("trajectories", storage_path="./data")
await q.put({"run_id": "r1", "step": 1, "reward": 0.5})
await q.flush()
records = await q.get(limit=100)
```

→ [Tensor Memory Guide](guide/tensor-memory.md) for full details on backends, dimensions, and block storage.

---

## Tools on the Same Foundation

### Agent Capture

Record every LLM call — Claude Code, Codex, or custom scripts:

```bash
persisting traj capture -o ./store -c proxy.toml -f md -- claude
```

→ [Capture Guide](guide/capture.md)

### Queues & KV API

Append/consume events with TransferQueue-compatible APIs:

```python
from persisting import Queue, SequentialSampler

q = Queue("training_data", storage_path="./data")
reader = q.reader()

meta = await reader.get_meta(
    fields=["input_ids"], batch_size=32, task_name="train",
    partition_id="p0", sampler=SequentialSampler())
batch = await reader.get_data(meta, partition_id="p0")
```

→ [Queue Guide](guide/queue.md)

### Search

```python
from persisting.search import add_document, query

add_document("docs", "text to index...")
results = query("docs", "search query", mode="hybrid", k=10)
```

→ [Search Guide](guide/search.md)

### pPilot

Map-style tasks with checkpoint/resume:

```bash
persisting ppilot task.py -w 4 --check       # validate
persisting ppilot task.py -w 4 -- --n 1000   # run
```

→ [pPilot Guide](guide/ppilot.md)

---

## Next Steps

- [User Guide](guide/index.md) — in-depth guides
- [API Reference](api/index.md) — complete API docs
- [Design Docs](design/index.md) — architecture, TTAS, tiered storage
