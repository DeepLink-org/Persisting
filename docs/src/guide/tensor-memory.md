# Tensor Memory (TTAS)

> **Status**: 🧪 Experimental — `local` and `tiered` backends available. GPU tiering and cross-node distribution are planned.

AI workloads don't fit into flat key-value stores. A KV cache block is addressed by `(session, layer, head, time)`. A model parameter is addressed by `(param_id, shard)`. A trajectory event is addressed by `(run_id, time)`. These are all **multi-dimensional** — and until now, each has been stored in its own silo.

Persisting solves this with **TTAS** — the Tiered Tensor Address Space. One addressing model for all three workloads. One storage engine. One tiering strategy across GPU, host memory, and SSD.

---

## The Core Idea

Think of TTAS like numpy fancy indexing, but the data lives in tiered storage and only flows to you when you materialize it.

```
   declare dimensions        open a namespace        slice by subscript
   ──────────────────       ────────────────        ──────────────────
   session: str             kv = open(               h = kv["s1", 0, 0:512]
   layer: int                 "kvcache/v1",
   head:  int                 dims=(sess, layer,     arr = h.tensor()
   time:  int                 head, time),
                              backend="tiered",
                              shape=(100, 32, 8, 4096))
```

The subscript `kv["s1", 0, 0:512]` doesn't move any data. It creates a **Handler** — a promise that says "I want the data at these coordinates." Only when you call `.tensor()` does data actually flow from wherever it lives (host memory, SSD, eventually GPU or remote node) into your numpy array.

This separation of **addressing** from **materialization** is what enables tiering. The Handler doesn't care where the data is — the storage engine figures out the fastest path.

---

## Three Workloads, One Interface

### Parameters: Named Weights with Shards

Model weights are naturally multi-dimensional. Here's a 70B model with 100 named parameters, each sharded 8 ways:

```python
import persisting
from persisting.core import Dimension

PARAM_ID = Dimension("param_id", "str")
SHARD    = Dimension("shard", "int")

ps = persisting.open("params/llama-70b",
    dims=(PARAM_ID, SHARD),
    backend="tiered",     # block-tiered across host memory + SSD
    shape=(100, 8),
)

# Point queries on both dimensions — fast, predictable
embed_weight = ps["embed.weight", 0].tensor()
lm_head      = ps["lm_head.weight", 0].tensor()

# Write back (write-through: updates both host cache and SSD baseline)
ps["embed.weight", 0].put(updated_tensor)
```

Parameters are point-query-heavy — you always know exactly which weight and shard you need. TTAS routes these as batch mget operations internally.

### KV Cache: Sessions, Layers, Heads, and Time

This is where TTAS earns its name. KV cache has a **time** dimension that grows monotonically during generation, and you often need range scans ("give me tokens 0 through 512 for this session/layer/head"):

```python
SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
HEAD    = Dimension("head", "int")
TIME    = Dimension("time", "int")

kv = persisting.open("kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME),
    order_dim=TIME,          # the dimension we range-scan on
    backend="tiered",
    shape=(100, 32, 8, 4096),
    block_tokens=64,          # chunk TIME into 64-token blocks
)

# Write a computed KV block
kv["s1", 0, 2, 100].put(kv_tensor)

# Range query — "give me all tokens 0-512 for this session/layer/head"
h = kv["s1", 0, 2, 0:512]    # → Handler, no data moved yet

# Materialize
arr = h.tensor()              # BlockStore checks L1 cache, misses go to SSD
```

The `order_dim=TIME` with `block_tokens=64` tells the storage engine: "chunk this dimension into 64-element blocks, and optimize for range scans along it." When you ask for `0:512`, the engine knows to fetch 8 blocks and serves them from the fastest available tier.

**Prefetch** lets you pull blocks into host memory before you need them:

```python
# While processing tokens 0-512, prefetch 512-1024
kv.prefetch(("s1", 0, 2, 512:1024))

# ... some compute happens ...

# Block until prefetch completes, then read with zero SSD latency
kv.wait(("s1", 0, 2, 512:1024))
next_chunk = kv["s1", 0, 2, 512:1024].tensor()
```

### Trajectories (Planned)

Trajectory events — agent LLM calls, tool outputs, rewards — are addressed as `(run_id, time)`. They share the same TTAS model. Today, trajectories flow through the `persisting traj` CLI and Queue API; TTAS-native trajectory namespaces are on the roadmap for RL batch scan workloads.

---

## How Tiering Actually Works

When you call `h.tensor()`, the BlockStore follows a simple decision tree for each block in your range:

```
Is the block in L1 (host memory)?
  ├── Yes → read directly, ~100ns latency
  └── No  → fetch from L3 (SSD) into L1, then read, ~10μs latency
```

On write, `h.put(data)` uses **write-through**: data lands in both L1 and L3 immediately. This means every write is durable — if the process crashes, the SSD has your data.

| Tier | Role | Status |
|------|------|--------|
| L0 (GPU) | Hot cache for active inference blocks | Planned |
| L1 (Host) | Warm buffer, mmap-able | Available |
| L3 (SSD) | Durable baseline, Lance files | Available |
| Remote node | Cross-node sharing via Pulsing | Planned |

The key insight: **L3 is always the source of truth.** L1 and L0 are accelerations. If a block is evicted from L1, it still exists on SSD. This is what makes tiering transparent — you never worry about data loss from cache eviction.

---

## Choosing a Backend

### `backend="local"` — Prototyping

For small datasets or development, a flat numpy array in memory:

```python
kv = persisting.open("params/v1",
    dims=(PARAM_ID, SHARD),
    shape=(1000, 8),
    backend="local",
)
```

No tiering, no blocks. Good for testing dimension layouts and subscript patterns before scaling up.

### `backend="tiered"` — Production

Block-granularity tiered storage. Adds ~10 lines to the `local` version:

```python
kv = persisting.open("kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME),
    order_dim=TIME,          # ← new: which dimension gets chunked
    backend="tiered",        # ← new: enable tiering
    shape=(100, 32, 8, 4096),
    block_tokens=64,          # ← new: chunk size
)
```

---

## Defining Dimensions

Dimensions are the schema of your namespace. Each has a name and a type:

```python
from persisting.core import Dimension

SESSION = Dimension("session", "str")   # string coordinates
LAYER   = Dimension("layer", "int")     # integer coordinates
TIME    = Dimension("time", "int")      # integer, supports range queries
```

| Kind | Range queries | Needs catalog |
|------|:---:|:---:|
| `"int"` | ✅ | No |
| `"str"` | ❌ | Yes (string → integer index) |
| `"bytes"` | ❌ | Yes |

For string dimensions, supply a catalog that maps coordinate names to integer indices:

```python
catalog = {SESSION: {"s1": 0, "s2": 1, "s3": 2}}
kv = persisting.open("kvcache/v1",
    dims=(SESSION, TIME), shape=(3, 512),
    catalog=catalog,
)
```

---

## Subscript Reference

| Form | Example | Meaning |
|------|---------|---------|
| Value | `"s1"`, `0` | Exact match on this dimension |
| Colon | `:` | All values (no constraint) |
| Range | `0:512` | Half-open `[0, 512)` — int only |

Two equivalent styles:

```python
kv["s1", 0, 2, 0:512]                        # positional
kv[{SESSION: "s1"}, :, :, slice(0, 512)]      # dict with slice object
kv[{SESSION: "s1"}, :, :, 0:512]              # dict, range shorthand
```

---

## Custom Storage Backings

Replace the default in-memory numpy array with mmap, safetensors, or remote backends:

```python
from persisting.store import LocalTensorStore, MmapBacking, SafetensorsBacking

# Memory-mapped file — share across processes
store = LocalTensorStore(DIMS, SHAPE, backing=MmapBacking(SHAPE, path="/data/cache.bin"))

# safetensors — load model weights directly
store = LocalTensorStore(DIMS, SHAPE, backing=SafetensorsBacking(SHAPE, path="/data/params.safetensors"))
```

For tiered backends, you can replace L1 and L3 independently:

```python
from persisting.store import BlockStore, MmapBacking, RemoteBacking

store = BlockStore(
    DIMS, SHAPE,
    order_dim=TIME, block_tokens=64,
    l1_backing=MmapBacking(SHAPE, path="/fast-nvme/l1.bin"),
    l3_backing=RemoteBacking(SHAPE, get_block=fetch_from_remote, put_block=push_to_remote),
)
```

This is how you'd connect a remote SSD or object store as the L3 baseline.

---

## Under the Hood

For most users, `kv[key].tensor()` and `kv[key].put(data)` are the entire surface. If you need to do routing, planning, or batch optimization, the underlying **Region** abstraction is available:

```python
from persisting.core import TensorView, canonicalize, is_point_query, is_range_query

tv = TensorView(DIMS)
region = tv["s1", :, :, 0:100]
region = canonicalize(region)              # normalize constraints
key = region.project_prefix([SESSION])     # → ("s1",) for routing
assert is_range_query(region, TIME)        # → True
```

→ [TTAS Design Document](../design/ttas.md) for the full formal model.

---

## What's Next

- **GPU tiering (L0)** — CUDA virtual memory mapping for hot KV cache blocks
- **Cross-node TTAS** — Pulsing-powered routing with RDMA data plane
- **Trajectory namespaces** — `persisting.open("trajectories/v1", dims=(RUN_ID, TIME))` for RL batch scan
- **`persisting.pin()`** — explicit pin/unpin for eviction control (spec exists, awaiting implementation)

---

## Next Steps

- [API Reference — Tensor Memory](../api/tensor-memory.md) — all method signatures
- [TTAS Design Document](../design/ttas.md) — formal addressing model
- [Distributed Tiered Storage](../design/distributed-tiered-storage.md) — block model, mmap+UFFD, cross-node
