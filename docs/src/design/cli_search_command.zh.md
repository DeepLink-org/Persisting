# `persisting search` 命令设计说明

`persisting search` 面向 **Agent Search** 场景，提供数据导入、索引维护、检索三类操作。

---

## 1. 命令树

```
persisting
└── search
    ├── create      # 从 JSONL / CSV / Lance 导入文档
    ├── index
    │   ├── list    # 列出索引
    │   ├── build   # 构建向量索引
    │   ├── delete  # 按名称删除索引
    │   ├── rebuild # 重建 / 合并索引
    │   └── reorder # 物理重排
    └── query       # 向量 / 全文 / 混合检索
```

---

## 2. 全局选项

| 选项 | 环境变量 | 说明 |
|------|----------|------|
| `--core-lib <PATH>` | `PERSISTING_ENGINE_LIB` | 引擎动态库路径 |

`--core-lib` 的路径解析与加载策略见 [CLI 整体架构设计](cli_architecture.zh.md)。`search` 层本身无额外选项。

---

## 3. 子命令

### 3.1 `persisting search create`

向指定 dataset 批量导入文档。数据来源支持 JSONL、CSV 或已有 Lance dataset。

```
persisting search create <DATASET> --input <PATH> [OPTIONS]
```

#### 参数

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `DATASET` | `String` | 是 | — | 目标 dataset 路径（位置参数） |
| `--input` | `String` | 是 | — | 输入源：文件路径、目录（Lance）、或 `-`（stdin） |
| `--format` | `auto` \| `jsonl` \| `csv` \| `lance` | 否 | `auto` | 输入格式；stdin 时须显式指定 |
| `--embedding-dim` | `usize` | 否 | `384` | embedding 维度 |
| `--lance-text-column` | `String` | 否 | `text` | Lance 导入时的文本列名 |
| `--lance-id-column` | `String` | 否 | — | Lance 导入时的 ID 列名（可选） |
| `--import-limit` | `usize` | 否 | — | 导入行数上限（可选） |

#### `--format auto` 推断规则

- `--input` 为目录且含 `data.lance/` → **lance**
- 扩展名 `.json` / `.jsonl` → **jsonl**，`.csv` → **csv**
- stdin（`-`）不推断，必须显式指定

#### 输入格式

**JSONL**：每行一个 JSON 对象，空行跳过。必须含 `text` 字段，可选 `id`。若含 `metadata` 键，其值作为顶层元数据。

**CSV**：首行为表头，必须含 `text` 列（大小写不敏感），可选 `id` 列。其余列并入 `metadata`。

**Lance**：单次整表导入。

---

### 3.2 `persisting search index`

管理 dataset 上的索引。

#### 3.2.1 `list` — 列出索引

```
persisting search index list <DATASET>
```

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `DATASET` | `String` | 是 | Dataset 路径（位置参数） |

#### 3.2.2 `delete` — 删除索引

```
persisting search index delete <DATASET> <INDEX_NAME>
```

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `DATASET` | `String` | 是 | Dataset 路径（位置参数） |
| `INDEX_NAME` | `String` | 是 | 索引名称（位置参数） |

#### 3.2.3 `rebuild` — 重建 / 合并索引

```
persisting search index rebuild <DATASET> [OPTIONS]
```

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `DATASET` | `String` | 是 | — | Dataset 路径（位置参数） |
| `--index-name` | `String` | 否 | — | 指定索引名；省略则操作全部非系统索引 |
| `--no-retrain` | flag | 否 | 关 | 仅合并不重训 |
| `--merge-num-indices` | `usize` | 否 | — | 合并的索引段数（仅 `--no-retrain` 时生效） |

#### 3.2.4 `build` — 构建 IVF-PQ 向量索引

```
persisting search index build <DATASET> [OPTIONS]
```

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `DATASET` | `String` | 是 | — | Dataset 路径（位置参数） |
| `--vector-column` | `String` | 否 | `embedding` | 向量列名 |
| `--text-column` | `String` | 否 | `text` | 全文列名 |
| `--metric` | `String` | 否 | `cosine` | 距离度量 |
| `--num-partitions` | `usize` | 否 | — | IVF 分区数 |
| `--ivf-max-iters` | `usize` | 否 | — | IVF k-means 最大迭代次数 |
| `--ivf-sample-rate` | `usize` | 否 | — | IVF 采样率 |
| `--ivf-target-partition-size` | `usize` | 否 | — | 目标每分区行数 |
| `--pq-num-sub-vectors` | `usize` | 否 | — | PQ 子向量数 |
| `--pq-num-bits` | `u8` | 否 | — | PQ 位数 |

#### 3.2.5 `reorder` — 物理重排

```
persisting search index reorder <DATASET> <PIVOT_INDEX> [OPTIONS]
```

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `DATASET` | `String` | 是 | 目标 dataset（位置参数） |
| `PIVOT_INDEX` | `String` | 是 | 驱动重排的 IVF 索引名（位置参数） |
| `--target` | `String` | 否 | 目标 URI |
| `--in-place` | flag | 否 | 原地重写 |

---

### 3.3 `persisting search query`

对 dataset 做检索，返回 top-k 结果。

```
persisting search query <DATASET> <QUERY> [OPTIONS]
```

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `DATASET` | `String` | 是 | — | Dataset 路径（位置参数） |
| `QUERY` | `String` | 是 | — | 查询文本（位置参数） |
| `--mode` | `vector` \| `fts` \| `hybrid` | 否 | `hybrid` | 检索模式 |
| `--k` | `usize` | 否 | `10` | 返回结果数 |
| `--embedding-dim` | `usize` | 否 | `384` | 查询向量维度 |
| `--filter` | `String` | 否 | — | 过滤表达式（预留） |
| `--nprobes` | `usize` | 否 | — | IVF 探测数（预留） |

---

## 4. 设计约定

- **资源锚点前置**：以 dataset 为操作目标的子命令，`DATASET` 均作为第一个位置参数。
- **其余为长选项**：可选参数统一使用 `--long` 形式。
- **stdin 格式须显式指定**：`--input -` 时 `--format` 不可省略。
- **输出到 stdout**：成功时 pretty 打印结果，失败时打印错误信息并返回非零退出码。
