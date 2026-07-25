# API Reference

This section documents public Python APIs. APIs marked experimental have a
smaller compatibility commitment and should not be treated as the shared
runtime for every Persisting capability.

| Module | Use for | Status |
|--------|---------|--------|
| [Tensor Memory](tensor-memory.md) | `persisting.open()` — local/tiered tensor namespace API | 🧪 Experimental |
| [Queue](queue.md) | `persisting.Queue` — event streaming, KV interface, samplers | ✅ Stable |
| [Search](search.md) | `persisting.search` — document indexing and retrieval | ✅ Stable |
| [Tensor Address Space](tensor-address-space.md) | `persisting.core` — direct access to Dimension, Region, canonicalization | 🧪 Experimental |

---

## Tensor Memory

```python
persisting.open(namespace, dims, ...) → TensorNamespace
kv[key]                              → Handler
h.tensor()                           → ndarray
h.put(data)                          → None
```

Current namespace examples:

| Namespace | Dims | Use |
|-----------|------|-----|
| `params/llama-70b` | `(param_id, shard)` | Model weights |
| `kvcache/v1` | `(session, layer, head, time)` | KV cache |

Trajectory capture uses its own Lance/Markdown storage model today; it does
not expose a tensor namespace through this API.

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
