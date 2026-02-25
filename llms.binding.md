# LLM Bindings — Persisting API Contract

**Canonical contract** for Persisting’s Python API. Code and docs (e.g. `api_reference.md`, `persisting/__init__.py`) are derived from this file; on mismatch, this file is authoritative.

- **Stability**: Tensor memory API (§2) and Conventions (§5) are the stable contract. Queue API (§3) and Advanced (§4) may evolve; we will call out breaking changes.
- **Scope**: Persisting only (data plane: tiered tensor memory + queues). Runtime (Pulsing actor/messaging) has its own binding/reference.

---

## 1. Overview

**Persisting** provides **distributed tiered memory** for AI workloads: parameters, KV cache, trajectories. Data is addressed by **multi-dimensional tensor subscript**; it lives across GPU, host memory, and SSD; **materialization** pulls it on demand.

- **Control plane**: Pulsing (actor discovery, messaging).
- **Data plane**: Persisting (TTAS addressing, tiering, placement).

**Two main surfaces:**

| Surface | Purpose |
|--------|---------|
| **Tensor memory** | Multi-dim namespace: `open` → `kv[key]` → `h.tensor()` / `h.put(data)` |
| **Queue** | Append / consume streams: `Queue`, `Writer`/`Reader`, `get_meta`/`get_data`, `KVInterface`, samplers |

---

## 2. Tensor Memory API (Stable)

### 2.1 Flow

All tensor access follows: **open** → **subscript slice** → **materialize or put**.

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
    backend="local",   # or "tiered"
    shape=(100, 32, 8, 4096),
    dtype=None,        # default numpy.float32
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
| `namespace` | yes | Namespace name (e.g. `"kvcache/v1"`). |
| `dims` | yes | Tuple of `Dimension` in order. |
| `order_dim` | tiered | Dimension used for range scan; required when `backend="tiered"`. |
| `partition_dims` | no | Dimensions used for cross-node partitioning. |
| `backend` | no | `"local"` (default) or `"tiered"`. |
| `shape` | yes | Tuple of sizes, one per dimension. |
| `dtype` | no | Array dtype (default `numpy.float32`). |
| `catalog` | no | For str/bytes dims: coordinate → index mapping. |
| `block_tokens` | no | For tiered: tokens per block on `order_dim` (default 64). |

**Returns**: A namespace handle supporting `kv[key]` (subscript). Each `kv[key]` returns a **Handler**.

### 2.3 Subscript rules

One value per dimension, in `dims` order:

| Form | Meaning |
|------|---------|
| `value` (e.g. `"s1"`, `0`) | Point constraint. |
| `:` | Unconstrained (that dimension). |
| `lo:hi` | Range `[lo, hi)`; int dimensions only. |

Examples: `kv["s1", 0, 2, 0:512]`, `kv[{SESSION: "s1"}, :, :, 0:512]`.

### 2.4 Handler

| Operation | Description |
|-----------|-------------|
| `h = kv[key]` | New Handler for the slice (no data copy). |
| `arr = h.tensor()` | Materialize: read from tiered storage into array. |
| `h.put(data)` | Write array into the slice’s address. |

- **Immutability**: Every `kv[key]` returns a **new** Handler.
- **Copy only on materialize**: Only `h.tensor()` moves data; subscript is address-only.

### 2.5 Prefetch / wait (when supported)

When the backend supports it (e.g. `backend="tiered"`), the namespace may expose:

- `kv.prefetch(key)` — request that the slice be brought into a faster tier.
- `kv.wait(key)` — block until that slice is ready (e.g. after prefetch).

Details are backend-specific; absence of these methods means no prefetch/wait.

### 2.6 Tiering (semantic)

Tiers are **transparent** to the user: `h.tensor()` reads from the fastest available tier; `h.put(data)` writes to the tier chosen by placement policy. User code does not choose tier explicitly.

| Tier | Role |
|------|------|
| GPU | Hot cache, limited capacity. |
| Host | Warm buffer. |
| SSD / NVMe | Cold, durable. |
| Remote node | Via Pulsing actors. |

### 2.7 Pin / Unpin (tiered, when eviction is enabled)

*Design direction; full alignment with two timelines and device-driven prefetch/eviction is not yet implemented.*

When the backend uses **bounded L1 + eviction** (e.g. `backend="tiered"` with `l1_max_blocks` and `eviction="lru"`), pin/unpin is available as follows.

**Context manager (module-level)**  
`persisting.pin(ref, after=None)` — `ref` is a slice reference (e.g. the **Handler** from `kv[key]`). Optional `after` binds unpin to a **completion event** (e.g. a GPU stream).

- **On enter**: the runtime pins `ref` — notifies the block manager to pin all blocks covering that slice (per-block ref-count += 1). Those blocks are not evicted while pinned.
- **On exit** (always, including on exception):  
  - If **no `after`**: the runtime schedules unpin asynchronously (e.g. launches operators that notify the block manager to unpin those blocks; ref-count -= 1).  
  - If **`after` is provided** (e.g. a GPU stream): the runtime does **not** unpin on Python exit. It **registers** an action so that when the work that **uses** `ref` on that stream has completed, an operator (e.g. on the same stream or a callback triggered by it) runs and notifies the block manager to unpin. So unpin happens only after the GPU (or other async executor) has finished using the data — the “unpin” is effectively performed by or right after that executor.

```python
# Synchronous / CPU: unpin when the with block exits
ref = kv["s1", 0, 0:512]
with persisting.pin(ref):
    x = ref.tensor()
    # ... use x ...
# exit: unpin is scheduled → block manager unpins the blocks for ref

# Asynchronous / GPU: unpin when the GPU work using ref has completed
with persisting.pin(ref, after=stream):
    x = ref.tensor()  # e.g. materialize to GPU
    stream.enqueue(kernel_that_uses_x, ...)
# exit: no unpin yet; an op is enqueued on stream to run after the above kernel,
#       and that op (or its completion) notifies the block manager to unpin
```

Semantics: the main guarantee is **automatic lifecycle management**. Blocks stay non-evictable while any reference (at any layer—e.g. the pin from `with`, derived GPU tensors, live Handlers) still uses the data; when those references are released or garbage-collected, the block may be evicted. The implementation achieves this via **multi-layer references and release/GC**: each layer (with exit, stream completion, tensor destruction, etc.) automatically decrements its hold; the block is only evictable when all holds are gone. Pin/unpin is one such layer; the user does not orchestrate “when to unpin” or “release order.” Ref-count is per block; multiple nested or overlapping pins add to the same block’s count. When `after` is given, the runtime defers that layer’s release until the given completion. If the backend has no eviction, `persisting.pin` may be a no-op or absent. **Effect**: this automatic memory management effectively increases usable (e.g. GPU) memory and supports better performance (throughput, larger batch, longer context). **Two timelines**: GPU memory state is not the same on the host timeline (release, GC) and the device timeline (kernels actually finished); release/eviction must respect both (e.g. `after=stream` ties release to device completion) so that memory is neither freed too early nor held too long. **Device-driven decisions**: the device should have the ability to drive or participate in prefetch and eviction decisions (e.g. callbacks or ops on the stream that notify the block manager), so that those actions align with the device timeline rather than only the host.

---

## 3. Queue API

Queue is for **streaming append** and **batch/stream consumption** (trajectories, events, or keyed tensor blobs).

### 3.1 Queue, Writer, Reader

```python
from persisting import Queue

queue = Queue(
    name="trajectories",
    storage_path="./data",
    batch_size=100,
    auto_flush_interval_sec=0.0,
    enable_metrics=False,
)
writer = queue.writer()
reader = queue.reader()

await writer.put({"id": "s1", "step": 1, "reward": 0.5})
await writer.put_batch([{"id": "s1", "step": 2, "reward": 0.8}, ...])
await writer.flush()

records = await queue.get(limit=100, offset=0)
```

### 3.2 Tensor-style read: get_meta + get_data

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

`BatchMeta`: `size`, `global_indexes`, `field_names`, etc.

### 3.3 Samplers

```python
from persisting import RankAwareSampler, GRPOGroupNSampler

rank_sampler = RankAwareSampler()
grpo_sampler = GRPOGroupNSampler(n_samples_per_prompt=4)
```

### 3.4 KVInterface (keyed tensor put/get on queue)

```python
from persisting import Queue, KVInterface

queue = Queue("kv_cache", storage_path="./data")
kv = KVInterface(queue)

await kv.kv_put("req-1", data=tensor_dict_1, partition_id="sess-a", tag={"role": "decode"})
await kv.kv_batch_put(["req-2", "req-3"], data=tensor_dict_batch, partition_id="sess-a", tags=[...])
data = await kv.kv_batch_get(["req-1", "req-3"], partition_id="sess-a", fields=["k", "v"])
pairs = await kv.kv_list("sess-a")
await kv.kv_clear(["req-2"], partition_id="sess-a")
```

### 3.5 Streaming read

```python
async for batch in queue.stream(limit=1000, wait=True, timeout=5.0):
    process(batch)
```

---

## 4. Advanced: TTAS (Tiered Tensor Address Space)

For routing, planning, or batch optimization, the addressing layer is exposed as TTAS (Tiered Tensor Address Space — the counterpart to PGAS). Implemented in the Rust crate **persisting-core**, public API is **persisting.core**:

```python
from persisting.core import (
    Dimension,
    TensorView,
    Region,
    canonicalize,
    project_prefix,
    is_point_query,
    is_range_query,
)

tv = TensorView(dims)
region = tv["s1", :, :, 0:100]
region = canonicalize(region)
assert is_range_query(region, TIME)
key = project_prefix(region, [SESSION, LAYER, HEAD])
```

TTAS is **internal** for most users; `kv[key].tensor()` is the primary surface.

---

## 5. Conventions (Stable)

- **Handler immutable**: each `kv[key]` returns a new Handler.
- **Copy only on materialize**: only `h.tensor()` moves data.
- **Dimension order**: fixed at `open()`; subscripts follow that order.
- **Range**: `lo:hi` is half-open `[lo, hi)`, int dims only.
- **Tiering**: transparent; no API to select tier.
- **Source of truth**: This document. Implementations and other docs must stay consistent with it.
