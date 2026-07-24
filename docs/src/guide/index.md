# User Guide

Persisting provides **unified tiered storage** for AI workloads. Trajectories, parameters, and KV cache share the same addressing model (TTAS), storage engine (Lance), and distribution runtime (Pulsing).

## Core: Unified Storage

| Guide | Workload | Dimensions | Status |
|-------|----------|-----------|--------|
| [Tensor Memory](tensor-memory.md) | Parameters, KV Cache, Trajectories | `(param_id, shard)`, `(session, layer, head, time)`, `(run_id, time)` | 🧪 Experimental |

All three workloads use `persisting.open()` with the same TTAS addressing. Block-tiered across host memory and SSD (GPU planned).

## Tools on the Same Foundation

| Guide | Description | Status |
|-------|-------------|--------|
| [Capture](capture.md) | Proxy and record LLM traffic — `persisting traj` | ✅ Stable |
| [Queue](queue.md) | Append/consume event streams, KV API, samplers | ✅ Stable |
| [Search](search.md) | Document indexing and vector/hybrid search | ✅ Stable |
| [Compute](compute.md) | Map-style task orchestration — `plan()` + `execute()` | ✅ Stable |
| [Custom Backends](custom-backends.md) | Implement your own storage backend | 📖 Reference |

---

## Architecture

```
┌───────────────────────────────────────────────────────┐
│  Trajectories      Parameters        KV Cache        │
│  (run_id, time)    (param_id, shard) (sess, layer, …)│
├───────────────────────────────────────────────────────┤
│                    TTAS                                │
│            Tiered Tensor Address Space                 │
├───────────────────────────────────────────────────────┤
│   Tiering:  GPU (L0)  ↔  Host (L1)  ↔  SSD (L3)     │
│   Route:    Pulsing actor runtime                      │
├───────────────────────────────────────────────────────┤
│            Lance Columnar Storage                       │
└───────────────────────────────────────────────────────┘
```

## Choosing Your Entry Point

| You want to… | Start with |
|--------------|------------|
| Store/retrieve parameters or KV cache by tensor subscript | [Tensor Memory](tensor-memory.md) |
| Record agent LLM calls | [Capture](capture.md) |
| Stream events with persistence | [Queue](queue.md) |
| Index and search documents | [Search](search.md) |
| Run batch jobs with checkpoint/resume | [Compute](compute.md) |
| Plug in custom storage | [Custom Backends](custom-backends.md) |
