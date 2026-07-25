# API 参考

本节记录公开 Python API。标为实验性的 API 兼容性承诺较小，不能视为所有
Persisting 能力的共享运行时。

| 模块 | 用途 | 状态 |
|---|---|---|
| [Tensor Memory](tensor-memory.md) | `persisting.open()` — local/tiered tensor namespace API | 🧪 实验性 |
| [Queue](queue.md) | `persisting.Queue` — 事件流、KV 接口、Sampler | ✅ 稳定 |
| [Search](search.md) | `persisting.search` — 文档索引与检索 | ✅ 稳定 |
| [Tensor Address Space](tensor-address-space.md) | `persisting.core` — Dimension、Region 与规范化 | 🧪 实验性 |

## Tensor Memory

```python
persisting.open(namespace, dims, ...) → TensorNamespace
kv[key]                              → Handler
h.tensor()                           → ndarray
h.put(data)                          → None
```

当前 namespace 示例：

| 命名空间 | 维度 | 用途 |
|---|---|---|
| `params/llama-70b` | `(param_id, shard)` | 模型权重 |
| `kvcache/v1` | `(session, layer, head, time)` | KV cache |

轨迹采集目前使用独立的 Lance/Markdown 存储模型，尚未通过此 API 暴露 tensor namespace。

→ [Tensor Memory API](tensor-memory.md)

## Queue 与事件

```python
Queue(name, storage_path, ...)
q.put(record) / q.get(limit)
KVInterface(q).kv_put / kv_batch_get
```

→ [Queue API](queue.md)

## 检索

```python
add_document(dataset, text)
query(dataset, query, mode, k)
```

→ [Search API](search.md)

## TTAS 类型

```python
Dimension(name, kind)
TensorView(dims)[key] → Region
canonicalize(region) / project_prefix(region, dims)
```

→ [Tensor Address Space API](tensor-address-space.md)
