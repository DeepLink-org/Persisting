# API 参考

Persisting 的 API 共享一个核心——TTAS 寻址、Lance 存储、Pulsing 分布式。

| 模块 | 用途 | 状态 |
|------|------|------|
| [Tensor Memory](tensor-memory.md) | `persisting.open()` — 参数、KV Cache、轨迹的统一存储 | 🧪 实验性 |
| [Queue](queue.md) | `persisting.Queue` — 事件流、KV 接口、Sampler | ✅ 稳定 |
| [Search](search.md) | `persisting.search` — 文档索引与检索 | ✅ 稳定 |
| [Tensor Address Space](tensor-address-space.md) | `persisting.core` — 直接访问 Dimension、Region、规范化 | 🧪 实验性 |

---

## 统一张量存储

```python
persisting.open(namespace, dims, ...) → TensorNamespace
kv[key]                              → Handler
h.tensor()                           → ndarray
h.put(data)                          → None
```

一套接口覆盖三类数据：

| 命名空间 | 维度 | 用途 |
|---------|------|------|
| `params/llama-70b` | `(param_id, shard)` | 模型权重 |
| `kvcache/v1` | `(session, layer, head, time)` | KV cache |
| `trajectories/v1`（规划中） | `(run_id, time)` | 轨迹张量访问 |

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
