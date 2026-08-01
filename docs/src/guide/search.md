# Agent Search

You've captured thousands of agent trajectories. You've indexed your documentation. Now you need to find things — "which session had that SSL error?", "show me all conversations about authentication", "find the document about proxy configuration."

Persisting Search gives you document indexing and retrieval on the same Lance engine that powers everything else. Import once, build an index, and query in three modes: vector similarity, full-text, or a hybrid of both.

---

## From Import to Query in Three Steps

### 1. Import Documents

Start with a Lance dataset — your documents. Import them from JSONL, CSV, or an existing Lance table:

```bash
# From JSONL (one JSON object per line, must have "text" field)
persisting search create ./docs --input docs.jsonl --format jsonl

# From CSV
persisting search create ./docs --input docs.csv --format csv

# From an existing Lance dataset
persisting search create ./docs --input existing.lance --format lance
```

Or from Python:

```python
from persisting.search import add_document, add_documents_batch, import_from_lance

# One document at a time
add_document("docs", "To configure the proxy, edit proxy.toml...",
    id="doc-1", metadata={"source": "readme"})

# Batch import
add_documents_batch("docs", [
    {"text": "Authentication requires an API key in the header...", "id": "doc-2"},
    {"text": "The proxy listens on 127.0.0.1:19081 by default...", "id": "doc-3"},
    {"text": "To capture trajectories, run `pvisor run`...", "id": "doc-4"},
], embedding_dim=384)

# From an existing Lance table
import_from_lance("docs", "source.lance",
    source_text_column="content", source_id_column="doc_id")
```

Under the hood, each document's text is embedded into a 384-dimensional vector. Documents and their vectors are stored in a Lance dataset — the same columnar format that backs Persisting's queues and trajectory storage.

### 2. Build an Index

Raw vector search works for small datasets. For anything beyond a few thousand documents, build an IVF-PQ index:

```bash
persisting search index build ./docs --num-partitions 100 --pq-num-sub-vectors 96
```

IVF-PQ (Inverted File with Product Quantization) partitions your vectors into clusters and compresses each vector. This trades a tiny amount of recall for orders-of-magnitude faster queries. The parameters above work well for most document collections; tune `num-partitions` (more = finer clusters) and `pq-num-sub-vectors` (more = better recall, larger index) for your data size.

In Python:

```python
from persisting.search import create_index

create_index("docs",
    vector_column="embedding",
    text_column="text",
    metric="cosine",
    num_partitions=100,
    pq_num_sub_vectors=96,
    pq_num_bits=8,
)
```

### 3. Query

```bash
persisting search query ./docs "how to configure the proxy" --mode hybrid --k 10
```

Three query modes:

| Mode | What it does | Best for |
|------|-------------|----------|
| `vector` | Compares embedding similarity | Semantic similarity — "find things like this" |
| `fts` | Full-text keyword matching | Exact terms — "documents mentioning 'SSL error'" |
| `hybrid` | Combines both, ranks by combined score | General search — best default |

In Python:

```python
from persisting.search import query

results = query("docs", "how to configure proxy",
    mode="hybrid",
    k=10,
)

for r in results["results"]:
    print(f"{r['text'][:80]}...  score={r['score']:.3f}")
```

---

## Managing Indexes

As your document collection grows, you'll need to maintain indexes:

```python
from persisting.search import list_indices, rebuild_indices, delete_index, reorder_ivf

# What indexes exist?
indices = list_indices("docs")

# Rebuild after adding many documents
rebuild_indices("docs", retrain=True)

# Merge index segments
rebuild_indices("docs", index_name="my_index", retrain=False, merge_num_indices=4)

# Physical reorder — rows with similar vectors placed together for better disk locality
reorder_ivf("docs", "my_index", in_place=True)

# Clean up
delete_index("docs", "old_index")
```

---

## How It Fits Together

Search shares the same Lance storage engine as Persisting's queues and trajectory capture. The architecture is straightforward:

```
Documents (JSONL/CSV/Lance)
    │
    ▼  import
Lance Dataset  ────  text column    ← full-text search can query directly
    │               embedding column ← vectors live here
    ▼  create_index
IVF-PQ Index  ────  partitions vectors for fast approximate search
    │
    ▼  query
Results (ranked by vector similarity, text relevance, or both)
```

The embedding step (`add_document`) calls into `persisting-engine` (Rust), which generates vectors and writes them alongside the text into a Lance table. Index building and querying also run in Rust via the engine, loaded by the CLI at runtime.

---

## Next Steps

- [API Reference — Search](../api/search.md) — all function signatures and parameters
- [Search CLI Design](../design/cli-search.md) — command structure and design choices
