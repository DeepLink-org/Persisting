# RFC-0015: `chronicle.manifest` Dataset Sidecar

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Format name** | `chronicle.manifest`（TOML） |
| **Date** | 2026-09-07 |
| **Component** | `persisting-pchronicle`、`pchronicle` CLI、Warehouse explorer |
| **Implements** | `crates/persisting-pchronicle/src/store/chronicle_manifest.rs` |
| **Related** | [RFC-0014 Compact JSONL](0014-compact-jsonl.md) · [RFC-0013 path Directory](0013-pchronicle-warehouse-catalog.md) · [RFC-0001 Storyline](0001-storyline-format.md) |

---

## 摘要

`chronicle.manifest` 是放在 Dataset 节点根目录的 **pChronicle 自有 TOML sidecar**。
它用于廉价发现，并保存常用聚合统计，使 Warehouse catalog / explorer 不必在每次刷新时
打开 Lance 或扫描全部记录。

本文使用 RFC 2119 的 **MUST**、**MUST NOT**、**SHOULD** 与 **MAY**。

## 动机

大型本地数据集（例如含大量行与 `_offload/` 的 compact-jsonl Lance 目录）目前会迫使
discovery 执行 `Dataset::open`，并迫使 explorer acceleration 物化逐条 run 摘要。当
Warehouse 挂载多个此类根目录时，约五秒一次的 catalog 刷新与 tree 轮询会让 serve 进程
持续高 CPU，即便用户只在浏览文件夹计数。

Lance 内部的 `_versions/*.manifest` 是 Lance 的 MVCC 控制面。pChronicle MUST NOT 在其中
编码应用层发现或 UI 统计。

pChronicle 已对其它布局使用应用控制文件（Storyline 的 `CURRENT`、Events 的
`_manifest.json`）。`chronicle.manifest` 把该模式扩展为可嵌套的 Dataset 节点描述符。

## 目标与非目标

目标：

- 定义稳定的文件名、TOML schema 与嵌套规则；
- 让 discovery 通过读小 TOML 文件即可分类 Dataset 节点；
- 持久化 explorer tree / dataset 摘要所需的聚合统计；
- 通过**自动扫描**子目录中的 `chronicle.manifest` 支持嵌套 Dataset 树；
- 无 manifest 的普通目录按 **Directory** 处理：只检查**一层**子目录是否为
  Dataset（manifest / `CURRENT` / events），不把松散文件登记为 Source；
- 在 sidecar 缺失时，仍可用 `CURRENT` / events / compact-jsonl 标记做 Dataset
  分类，但 MUST NOT 为分类而全量递归列举对象存储前缀。

非目标（v1）：

- 扩展 Lance protobuf manifest；
- 用 sidecar 替代 SQL 或详细 run/record 列表；
- 在父 manifest 中手写显式 `children` 列表；
- 把 Storyline `CURRENT` 或 Events `_manifest.json` 改写成此格式（它们仍是各自布局的权威；可选后续对齐）。

## 术语

- **Dataset 节点**：作为物理 source 根（leaf）或聚合嵌套 Dataset 节点（branch）的目录。
- **Leaf**：`format` 标识物理存储的节点（v1：`compact-jsonl/v1`）。
- **Branch**：用于嵌套子节点、没有物理 `format` 的节点。
- **Fingerprint**：把 `[stats]` 绑定到某一物理修订的字符串，供读者检测过期。

## 文件位置与名称

- 文件名 MUST 恰好为 `chronicle.manifest`。
- 文件 MUST 位于 Dataset 节点根（与 Lance `data/`、Storyline `CURRENT` 等并列）。
- 编码 MUST 为 UTF-8 TOML。

## 嵌套与发现

### 自动扫描子节点

父节点 MUST NOT 要求显式 children 列表。Discovery MUST：

1. 若当前目录存在 `chronicle.manifest`，则解析它；
2. 若 `kind = "leaf"`，将该目录视为对应 `format` 的一个 source 候选，且 MUST NOT 再递归其内部寻找其它 source；
3. 若 `kind = "branch"`，只扫描**一层**子目录；对每个含有 `chronicle.manifest` 的子目录，按该子节点的 kind 继续处理；
4. 若当前目录没有 `chronicle.manifest`，则视为 **Directory**：只检查**一层**
   子目录；对每个子目录用 Dataset 标记（`chronicle.manifest`、`CURRENT`、
   `events.lance/_manifest.json`、compact-jsonl Lance）分类。松散文件 MUST NOT
   登记为 Source。MUST NOT 为发现而递归列举整棵对象前缀树。

MUST 忽略符号链接。现有 `max_entries` / `max_files` 遍历上限仍然适用。

### Branch 聚合与轨迹计数

Branch 节点 MAY 省略 `[stats]`，且 MUST NOT 被当成轨迹 source。
只有 leaf 贡献轨迹数。

Catalog / explorer 展示文件夹合计（`run_count` / `record_count`）时：

- 读者 MUST 将某一 path 前缀的总数算为该前缀下**所有子孙 leaf** 的
  `[stats].record_count`（以及 `failed_count`）之和；
- 中间 branch 自身贡献 **0**，只负责嵌套；
- leaf MUST NOT 再向下递归寻找嵌套 source，避免物理 leaf 与子 leaf 双计。

#### 禁止祖先回写（写放大）

发布或更新某个 leaf 时，MUST **只**更新该 leaf 的 `chronicle.manifest`。
写入方 MUST NOT 为缓存累计总数而改写祖先 branch manifest。Branch 文件 SHOULD
仅作描述，例如：

```toml
schema_version = 1
kind = "branch"
```

文件夹累计数属于**读侧**职责。

#### 进程内刷新缓存

Warehouse / Catalog MAY 在进程内缓存已发现的 leaf stats 与前缀聚合（例如挂在
现有 catalog snapshot / acceleration 路径上），使周期性 UI 刷新不必重开 Lance
或重扫大目录树。当 leaf 的 `fingerprint` / manifest mtime 变化，或扫描前缀下
出现新的 `chronicle.manifest` 时，缓存条目 SHOULD 失效。进程缓存 MUST NOT
取代随数据一起分发的磁盘 leaf manifest 作为真相源。

## TOML schema（v1）

### 顶层必填

| 字段 | 类型 | 规则 |
|---|---|---|
| `schema_version` | integer | 本 RFC MUST 为 `1` |
| `kind` | string | MUST 为 `"leaf"` 或 `"branch"` |

### 仅 Leaf

| 字段 | 类型 | 规则 |
|---|---|---|
| `format` | string | `kind = "leaf"` 时 MUST 存在；v1 写入方 MUST 使用 `compact-jsonl/v1` |

未知 `format` 值 MUST 被通用读者保留；特定格式 opener MAY 拒绝不支持的值。

### `[identity]`

| 字段 | 类型 | 规则 |
|---|---|---|
| `fingerprint` | string | 存在 `[stats]` 时 MUST 存在；把 stats 绑定到物理修订 |

compact-jsonl v1 的 fingerprint SHOULD 为 `lance:version:<N>`，其中 `<N>` 是写入后发布的
Lance dataset version。

### `[stats]`

| 字段 | 类型 | 规则 |
|---|---|---|
| `record_count` | integer ≥ 0 | leaf compact-jsonl 写入方 MUST 提供 |
| `failed_count` | integer ≥ 0 | MUST 提供；未知/无失败时用 `0` |
| `min_timestamp` | string | MAY 省略 |
| `max_timestamp` | string | MAY 省略 |
| `total_tokens` | integer ≥ 0 | MAY 省略 |

后续 schema 版本 MAY 增加更多 stats 键；v1 读者 MUST 忽略 `[stats]` 下的未知键。

### Leaf 示例

```toml
schema_version = 1
kind = "leaf"
format = "compact-jsonl/v1"

[identity]
fingerprint = "lance:version:1"

[stats]
record_count = 12345
failed_count = 0
min_timestamp = "2026-01-01T00:00:00Z"
max_timestamp = "2026-09-07T01:00:00Z"
```

### Branch 示例

```toml
schema_version = 1
kind = "branch"
```

## 写路径

- `chronicle.manifest` 是 **store 层标准机制**：compact-jsonl 的唯一发布出口是
  `CompactJsonlStore::publish_manifest` / `import_path`。CLI `import` 与 `sync`
  MUST 只通过该 store API，不得在上层另写并行 sidecar。
- Compact JSONL `import` / 成功 republish / `sync` snapshot MUST 在输出 dataset 根写入
  `chronicle.manifest`。
- 本机文件系统上的写入 MUST 原子（写临时文件再 rename）。
- 物理写入成功后，`fingerprint` MUST 匹配已发布修订，且 `[stats].record_count` MUST 等于已发布行数。
- 若 dataset 写成功但 manifest 写失败，`import_path` MUST 失败（不发布半成品契约）；对
  仅 `ensure_manifest` 的只读升级路径，失败 MAY 记日志并继续打开物理数据。

## 读路径与过期

- 当 `fingerprint` 与已打开物理修订一致时，读者 MAY 信任 `[stats]` 做 explorer 聚合，而无需扫行。
- 文件缺失、不可读或 fingerprint 不匹配时，store 层 `ensure_manifest` SHOULD 就地补写；
  若补写失败，读者 MUST 回退现有 discovery / summary 路径。
- Manifest stats MUST NOT 成为查询正确性的唯一权威；SQL 与 record 列表仍读物理存储。

## 对 Warehouse explorer 的影响

- Catalog 刷新与 `/api/explorer/tree` 在可用时应优先用嵌套 manifest 做发现与文件夹
  `run_count` / `record_count` 聚合。
- 任意前缀上的文件夹 `run_count` MUST 等于其下子孙 leaf 轨迹权重之和（有 manifesto 时用
  `record_count`），而不是子 dataset 节点个数。
- 详细 run/record 页仍可打开物理 leaf；本 RFC 不要求 sidecar 索引每条 record 身份。

## 必需单元测试（v1）

实现 MUST 至少覆盖：

1. **嵌套发现**：`warehouse/(branch)` → `team/(branch)` → `codex_jsonl/(leaf, N)`
   恰好得到一条 compact source，路径为 `team/codex_jsonl`，`record_count = N`，且在
   leaf manifesto 存在时不打开 Lance。
2. **兄弟 leaf**：同一 branch 下两个 leaf，计数分别为 `A`、`B`，得到两条 source；
   tree 根 `run_count = A + B`；各子文件夹显示各自 leaf 总量。
3. **前缀上卷**：在 dataset 作用域内，前缀 `team` 聚合 `team/…` 下全部 leaf；前缀等于
   某 leaf 路径时只显示该 leaf。
4. **Leaf 不递归**：leaf 目录内即使还有嵌套 `chronicle.manifest`，也 MUST NOT 再为该
   子节点额外产出 source。
5. **写隔离（契约）**：只更新某一个 leaf 的 manifesto 后，重新 discovery 仍能通过读侧
   求和得到正确祖先总数，且无需改动父 branch 文件。

## 兼容性

- 没有 `chronicle.manifest` 的旧 dataset 仍然有效；首次经 store 打开或启发式 discovery
  确认 compact-jsonl 时，SHOULD 自动补写 sidecar。
- Lance schema metadata `pchronicle.format = compact-jsonl/v1` 仍是物理格式标记；sidecar 不替代它。
- 对象存储 URI 不在 v1 写入范围内；远端读取可在以后用同一 schema 扩展。对象存储上的嵌套
  branch 扫描 MAY 使用前缀列举 + 精确读取 `chronicle.manifest`；v1 不要求 S3 按文件名 glob 搜索。

## 曾考虑的替代方案

1. **扩展 Lance `_versions/*.manifest`** — 否决：二进制 MVCC、非 pChronicle 所有、不适合嵌套与 UI 统计。
2. **仅 Warehouse 内存缓存作为唯一统计存储** — 否决：不随数据走，进程重启即失效。进程缓存
   仍可作为磁盘 leaf manifesto 之上的**刷新优化**。
3. **父节点显式 `children` 列表** — 延后：自动扫描更贴合目录树，也避免子列表过期。
4. **把累计 `[stats]` 回写到每个祖先 branch** — v1 否决：单 leaf 更新会写放大，且易产生脏父节点。
