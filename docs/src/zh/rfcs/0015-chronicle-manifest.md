# RFC-0015: `chronicle.manifest` Dataset Sidecar

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Format name** | `chronicle.manifest`（TOML） |
| **Date** | 2026-09-07（2026-09-11 更新） |
| **Component** | `persisting-pchronicle`、`pchronicle` CLI、Warehouse Datasets UI |
| **Implements** | `crates/persisting-pchronicle/src/store/chronicle_manifest.rs` |
| **Related** | [RFC-0014 Compact JSONL](0014-compact-jsonl.md) · [RFC-0013 path Directory](0013-pchronicle-warehouse-catalog.md) · [RFC-0001 Storyline](0001-storyline-format.md) |

---

## 摘要

`chronicle.manifest` 是放在 Dataset 节点根目录的 **pChronicle 自有 TOML sidecar**。
它用于廉价发现，并保存常用聚合统计，使 `pchronicle ls` 和 Web Datasets 不必打开 Lance
或扫描全部记录即可分类目录，并展示类型 / 轨迹数预览。

本文使用 RFC 2119 的 **MUST**、**MUST NOT**、**SHOULD** 与 **MAY**。

## 动机

大型本地数据集（例如含大量行与 `_offload/` 的 compact-jsonl Lance 目录）目前会迫使
discovery 执行 `Dataset::open`，并迫使 Datasets UI 用 run 摘要反推文件夹。当
Warehouse 挂载多个此类根目录时，约五秒一次的 catalog 刷新与 tree 轮询会让 serve 进程
持续高 CPU，即便用户只在浏览一层目录。

Lance 内部的 `_versions/*.manifest` 是 Lance 的 MVCC 控制面。pChronicle MUST NOT 在其中
编码应用层发现或 UI 统计。

pChronicle 已对其它布局使用应用控制文件（Storyline 的 `CURRENT`、Events 的
`_manifest.json`）。`chronicle.manifest` 把该模式扩展为可嵌套的 Dataset 节点描述符。

## 目标与非目标

目标：

- 定义稳定的文件名、TOML schema 与嵌套规则；
- 让 discovery 通过读小 TOML 文件即可分类 Dataset 节点；
- 持久化 `ls` / Web Datasets 预览所需的聚合统计（类型与轨迹数）；
- 通过**自动扫描**子目录中的 `chronicle.manifest` 支持嵌套 Dataset 树；
- 把 **list** 和 **open** 分开：
  - `list(path)` 与 shell `ls` 一致：只列一层 children，MUST NOT 把嵌套 source 摊平；
  - `open(path)` 生成 Snapshot。叶子 Dataset 是一个 source。纯目录 MAY 被当成
    **虚拟 dataset** 打开：该路径下所有嵌套 Dataset 叶子与外围 JSON 合成一个查询空间。
- 在 sidecar 缺失时，仍可用 `CURRENT` / events / compact-jsonl 标记做 Dataset
  分类。`list` MUST NOT 为分类而全量递归列举对象存储前缀。对 Directory 做 `open`
  时 MAY 在现有 `max_entries` / `max_files` 上限内递归。

非目标（v1）：

- 扩展 Lance protobuf manifest；
- 用 sidecar 替代 SQL 或详细 run/record 列表；
- 在父 manifest 中手写显式 `children` 列表；
- 把 Storyline `CURRENT` 或 Events `_manifest.json` 改写成此格式（它们仍是各自布局的权威；可选后续对齐）。

## 术语

- **Dataset 节点**：作为物理 source 根（leaf）或聚合嵌套 Dataset 节点（branch）的目录。
- **Leaf**：`format` 标识物理存储的节点（v1：`compact-jsonl/v1` 或 `storyline/v1`）。
- **Branch**：用于嵌套子节点、没有物理 `format` 的节点。
- **Directory**：没有叶子 Dataset 标记的路径。对 `list` 而言是导航文件夹；仍 MAY 被
  `open` 成虚拟 dataset。这不是 RFC-0013 的 path Directory（名字 → path + ACL）。
- **list**：对某一 path 的一层列举。CLI `pchronicle ls` 与 Web Datasets MUST 共用此操作。
- **open**：把 path 钉成 Snapshot（`query` / `find` / `stats` / serve 挂载 / Open in Runs）。
- **虚拟 dataset**：`open` 一个 Directory 得到的 Snapshot：嵌套 Dataset 叶子，加上不在
  leaf 内部的 JSON / JSONL / NDJSON。
- **Fingerprint**：把 `[stats]` 绑定到某一物理修订的字符串，供读者检测过期。

## 文件位置与名称

- 文件名 MUST 恰好为 `chronicle.manifest`。
- 文件 MUST 位于 Dataset 节点根（与 Lance `data/`、Storyline `CURRENT` 等并列）。
- 编码 MUST 为 UTF-8 TOML。

## 嵌套与发现

`list` 与 `open` 共用分类规则，MUST NOT 共用结果形态。

路径按以下顺序判定为 **叶子 Dataset**：

1. `chronicle.manifest` 且 `kind = "leaf"`
2. `CURRENT`（Storyline）
3. `events.lance/_manifest.json`，或名为 `events.lance` 且含 `_manifest.json` 的目录
4. compact-jsonl Lance（`pchronicle.format = compact-jsonl/v1`，或 leaf sidecar
   `format = "compact-jsonl/v1"`）

否则该路径是 **Directory**（包括 `chronicle.manifest` 的 `kind = "branch"`）。

MUST 忽略符号链接。现有 `max_entries` / `max_files` 遍历上限仍然适用。
**`import` / `sync` 使用独立递归 JSON 扫描**，不受 `list` 约束。

### `list(path)` — 与 shell 一层列举一致

`pchronicle ls PATH` 与 Web Datasets MUST 只列出 `PATH` 的**一层** children，行为对齐
shell `ls`。MUST NOT 把嵌套 source 摊到当前列表。

每个 child 属于：

| 子项 | `list` kind | 预览 |
|---|---|---|
| 叶子 Dataset 目录 | dataset | `format`，以及 sidecar 中的 `[stats].record_count` / `failed_count` |
| Directory | directory | 仅名称；MUST NOT 为预览做深扫 |
| 本层 `.json` / `.jsonl` / `.ndjson` | file | 名称；`open` 前 format MAY 未知 |

对 **叶子 Dataset** 做 `list` 时 MUST NOT 列出 Lance 内部（`data/`、`_versions/`、
`generations/`、`_offload/` 等）。结果就是这一条 Dataset 及其预览。继续下钻是 `open` /
Runs，不是再一次 `ls`。

`pchronicle ls --sources` MUST 对 `path` 做 `open` 并打印 Snapshot 的 source 表（虚拟
dataset 成员）。默认 `ls` 仍是一层 children。`--sources` MUST NOT 改变 `list` 本身。

父节点 MUST NOT 要求 branch manifest 里写显式 children。对 branch 或 Directory 的
`list` 只看**一层**子项：有 Dataset 标记的列为 dataset，其余目录列为 directory，
本层松散 JSON 列为 file。

### `open(path)` — Snapshot，含虚拟 dataset

`open(path)` 构造查询 Snapshot：

1. 若 `path` 是叶子 Dataset，Snapshot 只有一个 source `_file_ = "."`。
   即使 leaf 内部还有嵌套 `chronicle.manifest`，也 MUST NOT 再为其产出 source。
2. 若 `path` 是 JSON / JSONL / NDJSON 文件，Snapshot 只有一个 file source。
3. 若 `path` 是 Directory，调用方 MAY **把它强行当作 query 根**。
   Snapshot 是 **虚拟 dataset**：在 `path` 下递归收集所有嵌套叶子 Dataset，以及
   **不在 leaf 内部**的 JSON / JSONL / NDJSON。未标注的中间目录本身不是 source，
   只构成 `_file_` 的路径段。`_file_` 相对这个 `path`。

serve 的每个挂载就是一次 `open(uri)`。进入挂载后的浏览是对 prefix 做 `list`。
**Open in Runs** SHOULD 沿用该 Snapshot，并 MAY 用 `_file_` 收窄。Runs 页 MAY 继续用
run 摘要重建路径树（`PathExplorer`）；那棵树不是 Datasets 的 `list`。
`pchronicle query ./warehouse/team` 是对该 URI 的另一次 `open`。

branch manifest 仍表示「此节点用于嵌套子节点」。对 branch 或无 sidecar 的 Directory
做 `open`，走同一套虚拟 dataset 遍历。对两者做 `list` 仍只列一层。

### Branch 聚合与轨迹计数

Branch 节点 MAY 省略 `[stats]`，且 MUST NOT 被当成轨迹 source。
只有 leaf 贡献轨迹数。

`open` 之后的 Dataset 预览，以及 `list` 上的叶子卡片：

- 叶子的轨迹数在 sidecar 存在且 fingerprint 可信时，用 `[stats].record_count`
  （以及 `failed_count`）；
- 对 Directory 做 `list` MUST NOT 上卷子孙计数（那是深扫）。上卷属于 Snapshot /
  `open`；
- 中间 branch 自身不作为 source；
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

Warehouse / Catalog MAY 在进程内缓存 `list` 的 children 与可信 leaf stats，使周期性
UI 刷新不必重开 Lance。当 leaf 的 `fingerprint` / manifest mtime 变化，或一层 children
中出现新的 `chronicle.manifest` 时，缓存条目 SHOULD 失效。进程缓存 MUST NOT
取代随数据一起分发的磁盘 leaf manifest 作为真相源。`list` 刷新 MUST NOT 跑
acceleration SQL 或 `steps` 的 token/duration 查询。

## TOML schema（v1）

### 顶层必填

| 字段 | 类型 | 规则 |
|---|---|---|
| `schema_version` | integer | 本 RFC MUST 为 `1` |
| `kind` | string | MUST 为 `"leaf"` 或 `"branch"` |

### 仅 Leaf

| 字段 | 类型 | 规则 |
|---|---|---|
| `format` | string | `kind = "leaf"` 时 MUST 存在；v1 写入方 MUST 使用 `compact-jsonl/v1` 或 `storyline/v1` |

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
- Storyline `import`（`--output-format storyline`）MUST 在每次分批 commit 后更新输出根上的
  leaf `chronicle.manifest`（`format = "storyline/v1"`，`record_count` 为已提交累计条数），
  以便 Warehouse catalog / explorer 在导入过程中观察到进展。
- 本机文件系统上的写入 MUST 原子（写临时文件再 rename）。
- 物理写入成功后，`fingerprint` MUST 匹配已发布修订，且 `[stats].record_count` MUST 等于已发布行数。
- 若 dataset 写成功但 manifest 写失败，`import_path` MUST 失败（不发布半成品契约）；对
  仅 `ensure_manifest` 的只读升级路径，失败 MAY 记日志并继续打开物理数据。

## 读路径与过期

- 当 `fingerprint` 与已打开物理修订一致时，读者 MAY 信任 `[stats]` 做 `ls` / Datasets
  预览，而无需扫行。
- 文件缺失、不可读或 fingerprint 不匹配时，store 层 `ensure_manifest` SHOULD 就地补写；
  若补写失败，读者 MUST 回退现有 discovery / summary 路径。
- Manifest stats MUST NOT 成为查询正确性的唯一权威；SQL 与 record 列表仍读物理存储。

## 对 Warehouse Datasets / `ls` 的影响

- CLI 默认 `ls` 与 `/api/explorer/tree`（或其后继）MUST 是同一个 `list(path)`：一层 children，
  Dataset 子项在有 `chronicle.manifest` 时带预览。
- `pchronicle ls --sources` MUST 打印 `open(path)` 的 Snapshot 成员。SQL
  `dataset.sources` 仍是查询内的等价视图。
- Web Datasets MUST NOT 用 `RunSummary`、`explorer_weight`、shallow-nav 回退或 `other`
  溢出桶反推文件夹。
- Runs 的 **Run paths** 树 MAY 继续按 `_file_` / import path 分组（run 反推树）。
  该面属于 Runs，不属于 Datasets。
- 单击 Directory 即导航（对该 prefix 再 `list`）。单击叶子 Dataset 不得钻进 Lance 内部。
  **Open in Runs** 对当前 query 根（serve 挂载）做 `open`，并 MAY 用 `_file_` 过滤。
- 详细 run/record 页仍可打开物理 leaf；本 RFC 不要求 sidecar 索引每条 record 身份。

## 必需单元测试（v1）

实现 MUST 至少覆盖：

1. **`list` 只列一层**：`warehouse/(Directory)` 下有 `team/(Directory)` 与
   `archive/(leaf, N)` 时，列出 `team/` 为 directory、`archive` 为 dataset 且
   `record_count = N`。MUST NOT 列出 `team/codex_jsonl`。
2. **`open` 是虚拟 dataset**：`open(warehouse)` 得到 `_file_ = team/codex_jsonl` 的
   compact source，`record_count = N`（以及其它兄弟 leaf / 不在 leaf 内的 JSON），
   且在 leaf manifesto 存在时不打开 Lance。
3. **对叶子做 `list`**：`list(archive)` 只报告这一条 Dataset 及预览，MUST NOT 列出
   `data/` 或 `_versions/`。
4. **`open` 时 Leaf 不递归**：leaf 目录内即使还有嵌套 `chronicle.manifest`，也 MUST NOT
   再为该子节点额外产出 source。
5. **写隔离（契约）**：只更新某一个 leaf 的 manifesto 后，无需改动父 branch 文件，该
   leaf 的 `list` 预览仍正确。
6. **CLI/UI 一致**：同一 path 上 Web Datasets 的 children 与 `pchronicle ls` 的名称、
   kind、Dataset 预览一致。
7. **`ls --sources`**：`ls --sources warehouse` 列出 `team/codex_jsonl`（以及其它
   Snapshot 成员），且 MUST 与 `open(warehouse)` / `dataset.sources` 一致。

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
5. **用 run 摘要 / acceleration 反推 Datasets 文件夹** — 否决用于 Datasets / 默认 `ls`。
   Runs 的 **Run paths** 树 MAY 继续按 run `_file_` / import path 分组。
6. **默认 `ls` 打印 Snapshot 成员** — 否决。默认 `ls` 对齐 shell。`ls --sources` 与
   `dataset.sources` 仍是 Snapshot 成员视图。
