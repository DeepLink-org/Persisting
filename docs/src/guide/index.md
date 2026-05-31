# User Guide

Welcome to the Persisting User Guide.

## What is Persisting?

Persisting provides persistent storage for parameters, KV Cache, and trajectories. Queues and Search use Lance; agent trajectory canonical storage is Vortex (`events.vortex`). Data lives across GPU, host memory, and SSD — addressed by tensor-style subscript, materialized on demand.

## Architecture

```
┌───────────────────────────────────────────────────────┐
│  Application                                          │
│  kv["s1", 0, 2, 0:512].tensor()                      │
├───────────────────────────────────────────────────────┤
│  Access Patterns                                      │
│  multi-dim lookup · streaming append · batch get      │
├───────────────────────────────────────────────────────┤
│  Persisting Core                                      │
│  TTAS (addressing) · Tiering (GPU/Host/SSD) · Route    │
├───────────────────────────────────────────────────────┤
│  Storage Engine: Lance                                │
│  columnar format · SSD persist · baseline read path   │
└───────────────────────────────────────────────────────┘
```

## Guides

### Tensor Memory (coming soon)

The primary API — tensor-style subscript access to tiered memory:

```python
kv = persisting.open("kvcache/v1", dims=(...), order_dim=TIME)
arr = kv["s1", 0, 2, 0:512].tensor()
```

### Capture (`traj`)

Proxy and record LLM traffic under **`persisting traj`**:

- **[Capture Quick Start](capture_quickstart.md)** — `traj capture`, `traj proxy`, inspect trajectories
- [Traj command](../design/cli_trajectory_command.zh.md)
- [Capture architecture](../design/capture_design.zh.md) (中文)

### Streaming Append (available now)

Lance storage engine's append-only access pattern — for event streaming and durable queues (trajectory capture uses Vortex separately; see Capture quick start).

- [Queue Backends](backends.md) — Overview of storage backends
- [Lance Backend](lance.md) — Using Lance for persistence
- [Persisting Backend](persisting.md) — Enhanced backend with metrics
- [Custom Backends](custom.md) — Implementing custom backends

## Next Steps

- For the tensor memory API, see the [design docs](../design/index.md) and the [TTAS specification](../design/tensor_address_algebra.md).
- For streaming append, continue with the [Queue Backends](backends.md) overview.
