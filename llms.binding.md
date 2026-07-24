# LLM Bindings — Persisting API Contract

**Canonical contract** for Persisting's Python API. On mismatch between this file and the implementation (`persisting/__init__.py`, etc.), this file is authoritative.

- **Stability**: Tensor Memory API (§2) and Conventions (§5) are the stable contract. Queue API (§3) and Advanced (§4) may evolve.
- **Scope**: Persisting data plane (tiered tensor memory + queues). Runtime (Pulsing) has its own binding.

---

## 1. Overview

**Persisting** provides **tiered memory** for AI workloads: parameters, KV cache, trajectories. Data is addressed by **multi-dimensional tensor subscript**; it lives across host memory and SSD (GPU planned); **materialization** pulls it on demand.

- **Control plane**: Pulsing (actor discovery, messaging).
- **Data plane**: Persisting (TTAS addressing, tiering, placement).

**Surfaces:**

| Surface | Purpose |
|--------|---------|
| **Tensor memory** | Multi-dim namespace: `open` → `kv[key]` → `h.tensor()` / `h.put(data)` |
| **Queue** | Append / consume streams: `Queue`, `KVInterface`, `get_meta`/`get_data`, samplers |
| **Search** | Document indexing and retrieval: `add_document`, `query`, `create_index` |

---

## 2. Tensor Memory API (Stable)

### 2.1 Flow

```python
import persisting
from persisting.core import Dimension

SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
HEAD    = Dimension("head", "int")
TIME    = Dimension("time", "int")

# 1) Open namespace
kv = persisting.open(
    "kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME),
    order_dim=TIME,
    partition_dims=(SESSION,),
    backend="tiered",
    shape=(100, 32, 8, 4096),
    dtype=None,
    catalog=None,
    block_tokens=64,
)

# 2) Slice (no copy)
h = kv["s1", 0, 2, 0:512]
h = kv[{SESSION: "s1"}, :, :, 0:512]

# 3) Materialize / write
arr = h.tensor()
h.put(tensor_data)
```

### 2.2 `persisting.open(...)`

| Argument | Required | Description |
|----------|----------|-------------|
| `namespace` | yes | Namespace name (e.g. `"kvcache/v1"`) |
| `dims` | yes | Tuple of `Dimension` in order |
| `order_dim` | tiered | Dimension for range scans and block partitioning |
| `partition_dims` | no | Dimensions for cross-node partitioning (planned) |
| `backend` | no | `"local"` (default) or `"tiered"` |
| `shape` | yes | Tuple of sizes, one per dimension |
| `dtype` | no | numpy dtype (default `float32`) |
| `catalog` | no | For str/bytes dims: coordinate → index mapping |
| `block_tokens` | no | For tiered: tokens per block on `order_dim` (default 64) |

**Returns**: `TensorNamespace` — supports `kv[key]` (subscript), `kv.prefetch(key)`, `kv.wait(key)`.

### 2.3 Subscript Rules

One value per dimension, in `dims` order:

| Form | Meaning |
|------|---------|
| `value` (e.g. `"s1"`, `0`) | Point constraint |
| `:` | Unconstrained |
| `lo:hi` | Range `[lo, hi)` — int dimensions only |

Examples: `kv["s1", 0, 2, 0:512]`, `kv[{SESSION: "s1"}, :, :, 0:512]`.

### 2.4 Handler

| Operation | Description |
|-----------|-------------|
| `h = kv[key]` | New Handler for the slice (no data copy) |
| `arr = h.tensor()` | Materialize: read from tiered storage into ndarray |
| `h.put(data)` | Write array into the slice's address |

- **Immutability**: Every `kv[key]` returns a **new** Handler.
- **Copy only on materialize**: Only `h.tensor()` moves data; subscript is address-only.

### 2.5 Prefetch / Wait (tiered backend)

```python
kv.prefetch(("s1", 0, 0:512))   # async pull blocks L3→L1
# ... do other work ...
kv.wait(("s1", 0, 0:512))       # block until ready
```

### 2.6 Tiering (Semantic)

Tiers are **transparent**: `h.tensor()` reads from the fastest available tier; `h.put(data)` writes to the tier chosen by placement policy.

| Tier | Role | Status |
|------|------|--------|
| GPU | Hot cache | Planned |
| Host | Warm buffer | Available |
| SSD / NVMe | Cold, durable | Available |
| Remote node | Via Pulsing actors | Planned |

---

## 3. Queue API

### 3.1 Queue

```python
from persisting import Queue

queue = Queue(
    name="trajectories",
    storage_path="./data",
    batch_size=100,
    auto_flush_interval_sec=0.0,
    enable_metrics=False,
)

# Write
await queue.put({"id": "s1", "step": 1, "reward": 0.5})
await queue.put_batch([{"id": "s1", "step": 2, "reward": 0.8}, ...])
await queue.flush()

# Read
records = await queue.get(limit=100, offset=0)

# Streaming
async for batch in queue.stream(limit=1000, wait=True, timeout=5.0):
    process(batch)
```

### 3.2 Tensor-style Read: get_meta + get_data

```python
from persisting import Queue, SequentialSampler

queue = Queue("tensor_queue", storage_path="./data")
reader = queue.reader()
sampler = SequentialSampler()

batch_meta = await reader.get_meta(
    fields=["input_ids", "attention_mask"],
    batch_size=32,
    task_name="actor_train",
    partition_id="train_0",
    sampler=sampler,
)
batch = await reader.get_data(batch_meta, partition_id="train_0")

# One-shot
batch2 = await reader.get_batch(
    fields=["input_ids", "attention_mask"],
    batch_size=32,
    task_name="actor_train",
    partition_id="train_0",
    sampler=sampler,
)
```

### 3.3 Samplers

```python
from persisting import RankAwareSampler, GRPOGroupNSampler

rank_sampler = RankAwareSampler()
grpo_sampler = GRPOGroupNSampler(n_samples_per_prompt=4)
```

### 3.4 KVInterface

```python
from persisting import Queue, KVInterface

queue = Queue("kv_cache", storage_path="./data")
kv = KVInterface(queue)

await kv.kv_put("req-1", data=tensor_dict_1, partition_id="sess-a", tag={"role": "decode"})
await kv.kv_batch_put(["req-2", "req-3"], data=tensor_dict_batch, partition_id="sess-a")
data = await kv.kv_batch_get(["req-1", "req-3"], partition_id="sess-a", fields=["k", "v"])
pairs = await kv.kv_list("sess-a")
await kv.kv_clear(["req-2"], partition_id="sess-a")
```

### 3.5 Distributed Mode

When Pulsing is initialized, Queue automatically switches to distributed mode. Same API, scaled across Pulsing actors with consistent hashing.

```python
queue = Queue("events",
    num_buckets=8,
    bucket_column="id",
    zerocopy_mode="auto",
)
```

---

## 4. Advanced: TTAS

For routing, planning, or batch optimization:

```python
from persisting.core import (
    Dimension, TensorView, Region, Point, Range,
    canonicalize, project_prefix,
    is_point_query, is_range_query,
)

tv = TensorView(dims)
region = tv["s1", :, :, 0:100]
region = canonicalize(region)
assert is_range_query(region, TIME)
key = project_prefix(region, [SESSION, LAYER, HEAD])
```

---

## 5. Conventions (Stable)

- **Handler immutable**: each `kv[key]` returns a new Handler.
- **Copy only on materialize**: only `h.tensor()` moves data.
- **Dimension order**: fixed at `open()`; subscripts follow that order.
- **Range**: `lo:hi` is half-open `[lo, hi)`, int dims only.
- **Tiering**: transparent; no API to select tier explicitly.
- **Source of truth**: This document. Implementation must stay consistent.
