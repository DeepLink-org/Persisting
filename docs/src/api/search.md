# Search API

```python
from persisting.search import (
    add_document,
    add_documents_batch,
    create_index,
    delete_index,
    embed_text,
    import_from_lance,
    list_indices,
    query,
    rebuild_indices,
    reorder_ivf,
)
```

---

## Document Management

### `add_document(dataset, text, *, id=None, metadata=None, embedding_dim=384)`

Index a single document.

```python
add_document("docs", "content here", id="doc-1", metadata={"source": "readme"})
```

### `add_documents_batch(dataset, documents, *, embedding_dim=384, chunk_size=256)`

Batch import. Each item is `{"text": ..., "id"?: ..., "metadata"?: ...}`.

```python
add_documents_batch("docs", [
    {"text": "Doc one", "id": "1"},
    {"text": "Doc two", "id": "2"},
])
```

### `import_from_lance(target_dataset, source_lance, *, source_text_column="text", source_id_column=None, embedding_dim=384, limit=None)`

Import from an existing Lance dataset.

```python
import_from_lance("target", "source.lance", source_text_column="content", limit=5000)
```

---

## Query

### `query(dataset, query, *, mode="hybrid", k=10, embedding_dim=384, text_column="text", filter=None, nprobes=None, minimum_nprobes=None, maximum_nprobes=None, adaptive_nprobes_margin=None)`

Search a dataset.

| Argument | Description |
|----------|-------------|
| `mode` | `"vector"`, `"fts"`, or `"hybrid"` |
| `k` | Number of results |
| `nprobes` | IVF probe count (optional) |
| `minimum_nprobes` / `maximum_nprobes` | Adaptive bounds |
| `adaptive_nprobes_margin` | Adaptive margin |

```python
results = query("docs", "search text", mode="hybrid", k=10)
for r in results["results"]:
    print(r["text"], r["score"])
```

---

## Index Management

### `create_index(dataset, *, vector_column="embedding", text_column="text", metric="cosine", ...)`

Build an IVF-PQ index.

```python
create_index("docs",
    vector_column="embedding",
    text_column="text",
    metric="cosine",
    num_partitions=100,
    pq_num_sub_vectors=96,
    pq_num_bits=8,
)
```

Full parameter list:

| Parameter | Type | Description |
|-----------|------|-------------|
| `num_partitions` | `int` | IVF partition count |
| `ivf_max_iters` | `int` | k-means max iterations |
| `ivf_balance_factor` | `float` | Balance factor |
| `ivf_balance_postprocess` | `bool` | Post-process balancing |
| `ivf_postprocess_max_cluster_ratio` | `float` | Max cluster ratio |
| `ivf_sample_rate` | `int` | Sample rate |
| `ivf_target_partition_size` | `int` | Target rows per partition |
| `ivf_shuffle_partition_batches` | `int` | Shuffle batch count |
| `ivf_shuffle_partition_concurrency` | `int` | Shuffle concurrency |
| `pq_num_sub_vectors` | `int` | PQ sub-vector count |
| `pq_num_bits` | `int` | PQ bits |
| `pq_max_iters` | `int` | PQ max iterations |
| `pq_kmeans_redos` | `int` | PQ k-means retries |
| `pq_sample_rate` | `int` | PQ sample rate |

### `list_indices(dataset)`

List non-system Lance index segments.

```python
indices = list_indices("docs")
```

### `delete_index(dataset, index_name)`

```python
delete_index("docs", "my_index")
```

### `rebuild_indices(dataset, *, index_name=None, retrain=True, merge_num_indices=None)`

Rebuild or merge indices.

```python
rebuild_indices("docs", index_name="my_index", retrain=True)
```

### `reorder_ivf(dataset, pivot_index, *, target=None, in_place=False)`

Physical reorder for better locality.

```python
reorder_ivf("docs", "my_index", in_place=True)
```

---

## Embedding

### `embed_text(text, embedding_dim=384)` → `list[float]`

```python
vec = embed_text("hello world", embedding_dim=384)
```
