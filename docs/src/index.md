---
template: home.html
title: Persisting — Persistent Storage for Trajectories, Parameters, and KV Cache
description: Unified tiered storage for AI workloads. Agent trajectories, model parameters, and KV cache share the same multi-dimensional addressing, Lance columnar engine, and Pulsing-powered distribution.
hide: toc
---

# Persisting

**Persistent Storage for Trajectories, Parameters, and KV Cache.**

Agent trajectories, model parameters, and KV cache share the same tiered storage — **one addressing model, one storage engine, one distribution runtime**. Data lives across GPU, host memory, and SSD, addressed by tensor subscript, materialized on demand.

---

## One Storage, Three Workloads

<div class="grid cards" markdown>

-   **📝 Agent Trajectories**

    ---

    Record every LLM call. Stored as `(agent_id, run_id, time)` tensors alongside canonical Vortex event logs and human-readable Markdown.

    ```bash
    persisting traj capture -o ./store -c proxy.toml -- claude
    ```

    → [Capture Guide](guide/capture.md)

-   **⚖️ Model Parameters**

    ---

    Address weights by name and shard. Tiered across host memory and SSD. Write-through to Lance baseline.

    ```python
    ps = persisting.open("params/llama-70b",
        dims=(PARAM_ID, SHARD), shape=(100, 8),
        backend="tiered")
    weights = ps["embed.weight", 0].tensor()
    ```

    → [Tensor Memory Guide](guide/tensor-memory.md)

-   **🧠 KV Cache**

    ---

    Cross-session, multi-layer KV cache. Block-tiered with prefetch. Same `(session, layer, head, time)` addressing as trajectories.

    ```python
    kv = persisting.open("kvcache/v1",
        dims=(SESSION, LAYER, HEAD, TIME),
        order_dim=TIME, backend="tiered",
        shape=(100, 32, 8, 4096))
    arr = kv["s1", 0, 2, 0:512].tensor()
    ```

    → [Tensor Memory Guide](guide/tensor-memory.md)

</div>

---

## Built on the Same Foundation

<div class="grid cards" markdown>

-   **📬 Streaming Queue**

    ---

    Append/consume event streams. Lance-backed, TransferQueue-compatible KV API, samplers.

    ```python
    q = Queue("events", storage_path="./data")
    await q.put({"step": 1, "reward": 0.5})
    ```

    → [Queue Guide](guide/queue.md)

-   **🔍 Agent Search**

    ---

    Import documents, build IVF-PQ indexes, vector/hybrid search. Same Lance engine as everything else.

    ```python
    from persisting.search import add_document, query
    results = query("docs", "how to configure proxy")
    ```

    → [Search Guide](guide/search.md)

-   **⚡ Compute Orchestration**

    ---

    Map-style task orchestration. `plan()` + `execute()`, local parallelism or torchrun.

    ```bash
    persisting compute task.py -w 4 -- --n 1000
    ```

    → [Compute Guide](guide/compute.md)

</div>

---

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  Trajectories        Parameters          KV Cache           │
│  (run_id, time)      (param_id, shard)   (sess, layer, …)  │
├──────────────────────────────────────────────────────────────┤
│                      TTAS                                    │
│              Tiered Tensor Address Space                     │
│         One addressing model for all AI data                 │
├──────────────────────────────────────────────────────────────┤
│   Tiering:  GPU (L0)  ↔  Host (L1)  ↔  SSD (L3)            │
│   Route:    Pulsing actor runtime                             │
├──────────────────────────────────────────────────────────────┤
│              Lance Columnar Storage                           │
│              SSD baseline for all workloads                   │
└──────────────────────────────────────────────────────────────┘
```

---

## Status

| Component | Status |
|-----------|--------|
| Trajectory Capture (`traj`) | ✅ Stable |
| Streaming Queue | ✅ Stable |
| Compute Orchestration | ✅ Stable |
| Agent Search | ✅ Stable |
| Tensor Memory (TTAS) | 🧪 Experimental |
| GPU Tiering / Cross-node | 📋 Planned |

---

## Quick Install

```bash
pip install persisting[lance]
```

→ [Installation Guide](installation.md) · [Quick Start](quickstart.md)
