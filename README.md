# Persisting

> Persistent Storage for Parameters, KV Cache, and Trajectories.

Persisting provides **distributed tiered memory** for AI workloads — data lives across GPU, host memory, and SSD, addressed by multi-dimensional tensor subscript, materialized on demand.

## How It Works

```
┌──────────────────────────────────────────────────────────┐
│                     Application                          │
│                                                          │
│   kv = persisting.open("kvcache/v1", dims, tiers)        │
│   h  = kv["s1", 0, 2, 0:512]    ← slice (no copy)      │
│   arr = h.tensor()               ← materialize (copy)   │
│                                                          │
├──────────────────────────────────────────────────────────┤
│  Persisting: Distributed Tiered Memory                   │
│                                                          │
│  ┌──────────┐   ┌──────────┐   ┌──────────────────────┐ │
│  │   TAA    │   │ Tiering  │   │  Placement / Route   │ │
│  │ (address │   │ GPU/Host │   │  (via Pulsing actors) │ │
│  │  algebra)│   │  /SSD    │   │                      │ │
│  └──────────┘   └──────────┘   └──────────────────────┘ │
│                                                          │
├──────────────────────────────────────────────────────────┤
│  Pulsing: Distributed Actor Runtime (control plane)      │
│                                                          │
│  actor discovery · messaging · lifecycle · gossip         │
└──────────────────────────────────────────────────────────┘
```

## Quick Start

```python
import persisting
from persisting.core import Dimension

SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
HEAD    = Dimension("head", "int")
TIME    = Dimension("time", "int")

# Open a tensor namespace with tiered storage
kv = persisting.open(
    "kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME),
    order_dim=TIME,
)

# Slice — no data copy, just an address descriptor
h = kv["s1", 0, 2, 0:512]

# Materialize — data flows from wherever it lives (GPU/host/SSD) into your buffer
arr = h.tensor()
```

## Why Not Just PGAS?

PGAS (UPC, Chapel, X10) gave HPC a global address space, but it assumed:
- **One extension dimension** (rank)
- **Linear addressing** (flat byte offset within a rank)
- **shmem API** (put/get)

AI workloads need something different:

| | PGAS | Persisting (TAA) |
|-|------|------------------|
| **Dimensions** | 1 (rank) | N (session, layer, head, time, shard, ...) |
| **Address model** | Linear | Multi-dimensional (point / range / set per dim) |
| **Access API** | shmem put/get | Tensor subscript: `kv["s1", 0, 2, 0:512]` |
| **Tiering** | None (flat memory) | GPU ↔ host ↔ SSD, policy-driven |
| **Distribution** | Language runtime | Pulsing actor runtime |

PGAS failed in HPC because "a nicer address space" alone doesn't move the Pareto curve. Persisting learns from that: the address algebra (TAA) is an **internal implementation detail** — what matters is the tiered memory performance it enables.

## Use Cases

### KV Cache Offloading (primary focus)

Cross-node KV Cache sharing with tiered storage. Same prefix computed once, stored once, accessible cluster-wide.

```python
kv = persisting.open("kvcache/v1", dims=(...), order_dim=TIME)

# Write: GPU → tiered storage
kv["s1", 0, 2, 100].put(kv_tensor)

# Read: tiered storage → GPU (from GPU cache / host / SSD, transparent)
arr = kv["s1", 0, 2, 0:512].tensor()
```

### Parameter Serving

Distributed model weights with shard-aware placement.

```python
ps = persisting.open("params/llama-70b", dims=(PARAM_ID, SHARD), ...)

weights = ps["embed.weight", 0].tensor()
```

### Trajectory Storage

RL trajectory collection with sequential scan.

```python
traj = persisting.open("trajectories/run1", dims=(RUN_ID, TIME), order_dim=TIME)

batch = traj["run_001", 0:1000].tensor()
```

## Storage Engine & Access Patterns

Lance 列式格式是 Persisting 的存储引擎——SSD 层的基础，也是最差情况的兜底（从文件重新读取）。上层所有优化（GPU 缓存、host 内存池、跨节点共享）都是对"从文件读"这个 baseline 的加速。

在同一个存储引擎之上，Persisting 提供不同的访问模式：

| Access Pattern | Scenario | Status |
|----------------|----------|--------|
| **Streaming append** | 轨迹收集、事件流（append-only + sequential scan） | Available |
| **Multi-dim lookup** | KV Cache（point query + range scan + prefetch） | In progress |
| **Batch point query** | Parameter serving（shard-aware batch get） | Planned |

### Streaming Append（已可用）

流式追加是轨迹/事件流场景的自然形态——Lance 存储引擎上的 append-only 队列：

```python
from persisting import Queue

queue = Queue("trajectories", storage_path="./data")
await queue.put({"step": 1, "reward": 0.5, "action": "left"})
await queue.put({"step": 2, "reward": 1.0, "action": "right"})
await queue.flush()

records = await queue.get(limit=100)
```

## Tiered Memory

| Tier | Latency | Capacity |
|------|---------|----------|
| GPU memory | ~μs | Small (hot cache) |
| Host memory | ~μs–ms | Medium (warm buffer) |
| SSD / NVMe | ~ms | Large (Lance storage engine) |
| Remote node | ~ms | Cluster-wide (via Pulsing) |

SSD 层（Lance）提供持久化兜底。上层 tier 是对它的逐级加速——数据放置和晋升/淘汰由 TAA 地址结构（分区键、有序维度）驱动。

## Installation

```bash
pip install persisting
```

## Roadmap

| Phase | Goal | Status |
|-------|------|--------|
| P0 | Lance 存储引擎 + TAA 寻址 + key encoding | In progress |
| P1 | KV Cache offloading (single node, GPU ↔ host ↔ SSD) | Next |
| P2 | Cross-node KV Cache sharing (via Pulsing) | Planned |
| P3 | Parameter serving + trajectory storage | Planned |

## License

Apache-2.0
