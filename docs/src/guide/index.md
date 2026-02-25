# User Guide

Welcome to the Persisting User Guide.

## What is Persisting?

Persisting provides persistent storage for parameters, KV Cache, and trajectories. Data lives across GPU, host memory, and SSD — addressed by tensor-style subscript, materialized on demand.

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
│  TAA (addressing) · Tiering (GPU/Host/SSD) · Route    │
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

### Streaming Append (available now)

Lance storage engine's append-only access pattern — for trajectory collection and event streaming. This is the currently available, production-ready capability.

- [Queue Backends](backends.md) — Overview of storage backends
- [Lance Backend](lance.md) — Using Lance for persistence
- [Persisting Backend](persisting.md) — Enhanced backend with metrics
- [Custom Backends](custom.md) — Implementing custom backends

## Next Steps

- For the tensor memory API, see the [design docs](../design/index.md) and the [TAA specification](../design/tensor_address_algebra.md).
- For streaming append, continue with the [Queue Backends](backends.md) overview.
