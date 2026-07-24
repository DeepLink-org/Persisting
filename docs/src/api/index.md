# API Reference

Persisting's APIs share a common core — TTAS addressing, Lance storage, Pulsing distribution.

| Module | Use for | Status |
|--------|---------|--------|
| [Tensor Memory](tensor-memory.md) | `persisting.open()` — unified storage for parameters, KV cache, trajectories | 🧪 Experimental |
| [Queue](queue.md) | `persisting.Queue` — event streaming, KV interface, samplers | ✅ Stable |
| [Search](search.md) | `persisting.search` — document indexing and retrieval | ✅ Stable |
| [Tensor Address Space](tensor-address-space.md) | `persisting.core` — direct access to Dimension, Region, canonicalization | 🧪 Experimental |

---

## Unified Tensor Storage

```python
persisting.open(namespace, dims, ...) → TensorNamespace
kv[key]                              → Handler
h.tensor()                           → ndarray
h.put(data)                          → None
```

One interface for all three workloads:

| Namespace | Dims | Use |
|-----------|------|-----|
| `params/llama-70b` | `(param_id, shard)` | Model weights |
| `kvcache/v1` | `(session, layer, head, time)` | KV cache |
| `trajectories/v1` (planned) | `(run_id, time)` | Trajectory tensor access |

→ [Tensor Memory API](tensor-memory.md)

## Queue & Events

```python
Queue(name, storage_path, ...)
q.put(record) / q.get(limit)
KVInterface(q).kv_put / kv_batch_get
```

→ [Queue API](queue.md)

## Search

```python
add_document(dataset, text)
query(dataset, query, mode, k)
```

→ [Search API](search.md)

## TTAS Types

```python
Dimension(name, kind)
TensorView(dims)[key] → Region
canonicalize(region) / project_prefix(region, dims)
```

→ [Tensor Address Space API](tensor-address-space.md)
