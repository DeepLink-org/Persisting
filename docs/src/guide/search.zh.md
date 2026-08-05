# Agent 检索

你采集了成千上万条 Agent 轨迹。你索引了文档。现在你需要找到东西——"哪个 session 出现过那个 SSL 错误？"，"给我看所有关于认证的对话"，"找到关于代理配置的文档。"

Persisting Search 在同一个 Lance 引擎上提供文档索引和检索——就是支撑 Persisting 其他一切的引擎。导入一次，构建索引，用三种模式查询：向量相似度、全文或混合检索。

Search 当前仅通过 Python API 暴露；统一 CLI 中原有的 `persisting search` 命令已经移除。

---

## 从导入到查询，三步走

### 1. 导入文档

从 Lance 数据集开始，通过 Python 导入文档：

```python
from persisting.search import add_document, add_documents_batch, import_from_lance

# 逐条添加
add_document("docs", "要配置代理，编辑 proxy.toml...",
    id="doc-1", metadata={"source": "readme"})

# 批量导入
add_documents_batch("docs", [
    {"text": "认证需要在 header 中携带 API key...", "id": "doc-2"},
    {"text": "代理默认监听 127.0.0.1:19081...", "id": "doc-3"},
    {"text": "采集轨迹运行 `pvisor run`...", "id": "doc-4"},
], embedding_dim=384)

# 从已有 Lance 表导入
import_from_lance("docs", "source.lance",
    source_text_column="content", source_id_column="doc_id")
```

底层，每个文档的文本被向量化为 384 维向量。文档和向量存入 Lance 数据集——与 Persisting 的 Queue 和轨迹存储相同的列式格式。

### 2. 构建索引

原始向量搜索对小型数据集够用。超过几千条文档后，构建 IVF-PQ 索引。

IVF-PQ（倒排文件 + 乘积量化）将向量分区聚类并压缩。牺牲微量召回率，换来数量级更快的查询。上面的参数适用于大多数文档集合；根据数据规模调整 `num-partitions`（更多 = 更细的聚类）和 `pq-num-sub-vectors`（更多 = 更好的召回，更大的索引）。

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

### 3. 查询

三种查询模式：

| 模式 | 做什么 | 最适合 |
|------|--------|--------|
| `vector` | 比较向量相似度 | 语义相似——"像这样的东西" |
| `fts` | 全文关键词匹配 | 精确术语——"提到 SSL 错误的文档" |
| `hybrid` | 结合两者，按综合分数排序 | 一般搜索——最佳默认 |

```python
from persisting.search import query

results = query("docs", "如何配置代理",
    mode="hybrid",
    k=10,
)

for r in results["results"]:
    print(f"{r['text'][:80]}...  分数={r['score']:.3f}")
```

---

## 管理索引

文档量增长时，需要维护索引：

```python
from persisting.search import list_indices, rebuild_indices, delete_index, reorder_ivf

# 有哪些索引？
indices = list_indices("docs")

# 添加大量文档后重建
rebuild_indices("docs", retrain=True)

# 合并索引段
rebuild_indices("docs", index_name="my_index", retrain=False, merge_num_indices=4)

# 物理重排——相似向量放一起，提升磁盘局部性
reorder_ivf("docs", "my_index", in_place=True)

# 清理
delete_index("docs", "old_index")
```

---

## 整体如何协作

Search 使用与 Persisting 的 Queue 和 Capture 相同的 Lance 存储引擎。架构简单直接：

```
文档 (JSONL/CSV/Lance)
    │
    ▼  import
Lance 数据集  ────  text 列       ← 全文搜索可直接查询
    │              embedding 列   ← 向量存这里
    ▼  create_index
IVF-PQ 索引  ────  分区向量，支持快速近似搜索
    │
    ▼  query
结果 (按向量相似度、文本相关性、或两者综合排序)
```

向量化步骤（`add_document`）、索引构建和查询都通过 Python 扩展调用 pChronicle 的 Rust Search 实现。

---

## 下一步

- [API 参考 — Search](../api/search.md) — 所有函数签名和参数
