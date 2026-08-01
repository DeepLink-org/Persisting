# Persisting

**Agent execution, orchestration, and durable history.**

Persisting's Agent infrastructure has three peer components: **pVisor** manages
one Agent Run, **pPilot** orchestrates many Runs, and **pChronicle** stores
canonical Run history. Gateway, OverlayNet, Control, and OverlayFS are runtime
drivers assembled by pVisor.

```text
pPilot ── RunSpec ──► pVisor ── EventRecord ──► pChronicle
  │                    │                         ▲
  └──── history ───────┴─────────────────────────┘
                       ├─ Control
                       ├─ OverlayFS
                       └─ OverlayNet → Gateway sink
```

---

## Experimental Unified Data-Plane Vision

```
┌─────────────────────────────────────────────────────────────────┐
│                      Application                                 │
│                                                                  │
│   pvisor run            persisting.open()       Queue           │
│   (agent proxy)         (tensor subscript)      (event stream)  │
│                                                                  │
├─────────────────────────────────────────────────────────────────┤
│                    Persisting Data Plane                         │
│                                                                  │
│   ┌─────────────────────────────────────────────────────────┐   │
│   │                      TTAS                                │   │
│   │         Tiered Tensor Address Space                      │   │
│   │                                                          │   │
│   │   Trajectories:  (run_id, time)                          │   │
│   │   Parameters:    (param_id, shard)                       │   │
│   │   KV Cache:      (session, layer, head, time)            │   │
│   │                                                          │   │
│   │   All share the same address model, same routing,        │   │
│   │   same block-tiering across GPU / host / SSD.            │   │
│   └─────────────────────────────────────────────────────────┘   │
│                                                                  │
│   Tiering:  GPU (L0)  ↔  Host (L1)  ↔  SSD (L3)                │
│   Route:    Pulsing actor runtime                                │
├─────────────────────────────────────────────────────────────────┤
│                     Storage Engine                               │
│                                                                  │
│   Lance (columnar)  ·  Lance (trajectory)  ·  Numpy (memory)  │
└─────────────────────────────────────────────────────────────────┘
```

The diagram above is a target for TTAS/tiered tensor storage, not the current
pChronicle trajectory architecture. Tensor Memory, Queue, and Search remain
additional capability-specific data systems.

---

## Quick Start

```bash
pip install persisting[lance]
```

### Agent Trajectories

Execute an Agent under pVisor and record its LLM calls:

```bash
pvisor run --workspace ./run \
  --overlaynet-mode proxy \
  --gateway-mode capture \
  --gateway-route 'name="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  --gateway-stream-markdown \
  -- claude
```

Trajectories are stored as `(agent_id, run_id, time)` — the same TTAS model used for KV cache and parameters.

### Model Parameters

Address weights by name and shard:

```python
import persisting
from persisting.core import Dimension

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

# Write: GPU → tiered storage
kv["s1", 0, 2, 100].put(kv_tensor)

# Read: from fastest available tier (CPU / SSD, GPU planned)
arr = kv["s1", 0, 2, 0:512].tensor()
```

---

## What's Available

| Capability | Status | Description |
|------------|--------|-------------|
| **Agent Trajectory Capture** | ✅ Stable | Proxy + record LLM traffic as Lance + Markdown |
| **Streaming Queue** | ✅ Stable | Lance-backed append/consume, KV API, samplers |
| **pPilot Orchestration** | ✅ Stable | `plan()` + `execute()`, local/torchrun |
| **Agent Search** | ✅ Stable | Document indexing, IVF-PQ, hybrid search |
| **Tensor Memory (TTAS)** | 🧪 Experimental | Multi-dim tensor subscript, tiered backends |
| **Cross-node KV Cache** | 📋 Planned | Pulsing + RDMA data plane |

---

## Why Persisting?

### One addressing model for all AI data

Trajectories, parameters, and KV cache aren't separate silos — they're all multi-dimensional data. Persisting uses **TTAS** (Tiered Tensor Address Space) as a single addressing model:

| Workload | Dimensions | Access Pattern |
|----------|-----------|----------------|
| KV Cache | `(session, layer, head, time)` | Point query + range scan + prefetch |
| Parameters | `(param_id, shard)` | Batch point query |
| Trajectories | `(run_id, time)` | Sequential range scan |

### Lance as the common baseline

All data — whether it's a trajectory event log, a parameter shard, or a KV cache block — lands on Lance columnar storage. Upper tiers (host memory, GPU) are accelerations built on top of that baseline, driven by the TTAS address structure.

### Pulsing-powered distribution

Cross-node routing and placement via [Pulsing](https://github.com/DeepLink-org/pulsing)'s actor runtime. Pulsing handles the control plane (discovery, messaging, lifecycle); Persisting handles the data plane.

---

## Storage Tiers

| Tier | Latency | Role | Status |
|------|---------|------|--------|
| GPU (L0) | ~μs | Hot cache | Planned |
| Host (L1) | ~100ns / ~10μs | Warm buffer | Available |
| Remote (L2) | ~2μs (RDMA) | Cross-node | Planned |
| SSD (L3) | ~10μs | Cold, durable baseline | Available |

---

## Installation

```bash
pip install persisting[lance]        # Full
pip install persisting               # Minimal
```

For the CLI tools (`persisting traj`, `persisting ppilot`, `persisting search`):

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting && cargo build -p persisting-pvisor --bin pvisor -p persisting-cli -p persisting-engine
```

---

## Documentation

| Document | Description |
|----------|-------------|
| [Quick Start](https://deepLink-org.github.io/Persisting/quickstart/) | Get started in 5 minutes |
| [User Guide](https://deepLink-org.github.io/Persisting/guide/) | Capture, tensor memory, queue, search |
| [API Reference](https://deepLink-org.github.io/Persisting/api/) | Full API documentation |
| [Design Docs](https://deepLink-org.github.io/Persisting/design/) | Architecture, TTAS, tiered storage |

---

## License

[GPL-3.0-or-later](LICENSE)

Persisting includes a vendored copy of
[fuse-overlayfs](https://github.com/containers/fuse-overlayfs)
(`crates/persisting-fs-overlay`, upstream GPL-2.0-or-later). See [`NOTICE`](NOTICE)
for third-party attribution.
