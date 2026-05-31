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
│  │   TTAS   │   │ Tiering  │   │  Placement / Route   │ │
│  │ (tiered  │   │ GPU/Host │   │  (via Pulsing actors) │ │
│  │  tensor) │   │  /SSD    │   │                      │ │
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

| | PGAS | TTAS (Persisting) |
|-|------|------------------|
| **Full name** | Partitioned Global **Address Space** | **T**iered **T**ensor **A**ddress **S**pace |
| **Dimensions** | 1 (rank) | N (session, layer, head, time, shard, ...) |
| **Address model** | Linear | Multi-dimensional (point / range / set per dim) |
| **Access API** | shmem put/get | Tensor subscript: `kv["s1", 0, 2, 0:512]` |
| **Tiering** | None (flat memory) | GPU ↔ host ↔ SSD, policy-driven |
| **Distribution** | Language runtime | Pulsing actor runtime |

PGAS failed in HPC because "a nicer address space" alone doesn't move the Pareto curve. Persisting learns from that: the **Tiered Tensor Address Space (TTAS)** is internal infrastructure — what matters is the tiered memory performance it enables.

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

### Trajectory Storage (available now)

Agent LLM traffic capture via **`persisting traj`**: Vortex canonical event log (`events.vortex`) plus Markdown materialization for human review.

```bash
# One-shot: proxy + your agent; default writes Markdown only (-f md)
persisting traj capture -o ./store -c proxy.toml -f md -- python3 agent.py

# Machine-readable canonical log (replay / materialize to .md later)
persisting traj capture -o ./store -c proxy.toml -f vortex -- python3 agent.py
```

See [Capture Quick Start](docs/src/guide/capture_quickstart.md) and [trajectory storage design](docs/src/design/trajectory_storage.zh.md) (中文).

**Planned (TTAS):** tensor-style trajectory namespaces for RL batch scan — same tiered-memory model as KV Cache.

## Storage Engine & Access Patterns

**Lance** 列式格式是队列、Search 与 SSD 持久化的共同底座——也是最差情况的兜底（从文件重新读取）。**Agent 轨迹 canonical** 使用 **Vortex**（`{run}/events.vortex`），Markdown 为物化人读视图；二者与 Lance 队列层独立。上层所有优化（GPU 缓存、host 内存池、跨节点共享）都是对「从文件读」这一 baseline 的加速。

| Access Pattern | Scenario | Status |
|----------------|----------|--------|
| **Agent capture** | LLM proxy + Vortex log + Markdown (`traj capture`) | Available |
| **Streaming append** | 事件流、队列持久化（append-only + sequential scan，Lance） | Available |
| **Multi-dim lookup** | KV Cache（point query + range scan + prefetch） | In progress |
| **Batch point query** | Parameter serving（shard-aware batch get） | Planned |

### Streaming Append（已可用）

流式追加适用于**队列与事件流**——Lance 存储引擎上的 append-only `Queue`（与轨迹 Vortex 层无关）：

```python
from persisting import Queue

queue = Queue("events", storage_path="./data")
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
| SSD / NVMe | ~ms | Large (Lance / Vortex on disk) |
| Remote node | ~ms | Cluster-wide (via Pulsing) |

SSD 层（Lance 队列、Vortex 轨迹等）提供持久化兜底。上层 tier 是对它的逐级加速——数据放置和晋升/淘汰由 TTAS 地址结构（分区键、有序维度）驱动。

## Installation

```bash
pip install persisting
```

## Roadmap

| Phase | Goal | Status |
|-------|------|--------|
| P0 | Lance 存储引擎 + TTAS 寻址 + key encoding | In progress |
| P0.5 | Agent trajectory capture（Vortex + Markdown，`traj` CLI） | **Shipped** |
| P1 | KV Cache offloading (single node, GPU ↔ host ↔ SSD) | Next |
| P2 | Cross-node KV Cache sharing (via Pulsing) | Planned |
| P3 | Parameter serving + TTAS trajectory namespaces | Planned |

## License

Apache-2.0
