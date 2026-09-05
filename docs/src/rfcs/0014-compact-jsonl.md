# RFC-0014: Compact JSONL Lance 存储格式

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Format name** | `compact-jsonl/v1` |
| **Date** | 2026-09-04 |
| **Component** | `persisting-pchronicle`、`pchronicle` CLI |
| **Implements** | `crates/persisting-pchronicle/src/store/compact_jsonl.rs` |
| **Related** | [RFC-0003 pChronicle ownership](0003-pchronicle-ownership.md) · [RFC-0010 Agent corpus Lance layout](0010-agent-corpus-lance-layout.md) |

---

## 摘要

Compact JSONL 把一个文件或目录树中的 JSONL 记录写入一个 Lance dataset，同时保留每条
记录的来源文件、可查询 JSONB、用户选择的投影列和逐字节导出所需的数据。它不把输入转换成
Storyline，也不推断轨迹语义。

本文使用 RFC 2119 的 **MUST**、**MUST NOT**、**SHOULD** 与 **MAY** 表示规范要求。

## 目标与非目标

目标：

- 一个输入 JSON object 对应一行 Lance 数据；
- `id`、`timestamp`、`filename` 和 `data` 构成稳定的最小 schema；
- 用户可用 `--column NAME=JSON_PATH` 指定 `id`、`timestamp` 或附加 JSONB 列；
- import、sync 和 export 支持完整目录，并逐字节恢复所有合法输入文件；
- 超过阈值的原始记录可放入内容寻址的 offload 对象；
- schema 可被独立 reader 识别和拒绝，而不依赖目录名。

非目标：

- 识别 ATIF、ACTF、Codex、Claude Code 或 Storyline 语义；
- 把任意 JSONPath 标准完整实现；
- 对 `timestamp` 的单位、时区或业务含义作推断；
- 在 v1 中支持远端对象存储、append 或行级增量 sync。

## 术语

- **input root**：import/sync 指定的文件或目录。
- **record**：JSONL 中一行 JSON object。
- **source filename**：record 所在文件相对于 input root 的 UTF-8 路径。
- **mapping**：列名到受限 JSON path 的映射。
- **raw record**：record 的全部原始字节，包括原有的 `LF` 或 `CRLF`；最后一行没有换行时
  也保持没有换行。
- **offload object**：dataset 下由 BLAKE3-256 key 标识的原始记录文件。

## 输入语法

### 文件集合

显式文件输入 MUST 是扩展名大小写不敏感的 `.jsonl` 普通文件。目录输入 MUST 递归选择
`.jsonl` 普通文件，并 MUST 忽略符号链接和其他扩展名。实现 MUST 按规范化相对文件名的
字节序处理文件；单个文件内部 MUST 按原始行顺序处理。

每个被选择的文件 MUST 至少包含一条 record。每一物理行 MUST：

1. 不是空行或纯空白行；
2. 是 UTF-8 JSON；
3. 顶层值是 JSON object；
4. 包含可映射为非空标量的 `id` 和 `timestamp`。

任一条件不满足时，整个 import/sync MUST 失败，不得发布部分 dataset。

### JSON path 子集

Compact JSONL v1 不声称实现通用 JSONPath。它只接受以下语法：

```abnf
path       = "$" / "$" 1*(member [index])
member     = "." identifier
identifier = (ALPHA / "_") *(ALPHA / DIGIT / "_")
index      = "[" 1*DIGIT "]"
```

示例：`$`、`$.id`、`$.user.id`、`$.messages[0].role`。

不支持 wildcard、slice、filter、递归下降、quoted member、负数 index 或一段中的多个 index。
映射缺失时，附加列写 Arrow null；`id` 或 `timestamp` 映射缺失时必须拒绝该 record。

### 列映射

默认映射为：

```text
id=$.id
timestamp=$.timestamp
```

`--column id=PATH` 和 `--column timestamp=PATH` 覆盖默认映射，但不增加重复物理列。其他
`--column NAME=PATH` 增加一个 nullable JSONB 列。

`NAME` MUST 符合上述 `identifier`。映射名 MUST 唯一。`filename`、`data`、`_raw_` 和
`_offload_` 是保留名，MUST NOT 用作用户映射名。

`id` 和 `timestamp` 的来源值 MUST 是 JSON string 或 number。string 原样写入；number 使用
JSON 规范表示写为 UTF-8。null、boolean、array、object 与空 string 均无效。dataset 内的
`id` MUST 唯一。

## Lance 物理 schema

Lance schema metadata MUST 包含：

| Key | Value |
|---|---|
| `pchronicle.format` | `compact-jsonl/v1` |
| `pchronicle.columns` | `CompactJsonlColumn[]` 的 JSON 编码；包括 `id`/`timestamp` 覆盖 |

列顺序和类型为：

| Ordinal | 列 | Arrow/Lance 类型 | Nullable | 语义 |
|---:|---|---|---|---|
| 0 | `id` | `Utf8` | no | record 的稳定唯一 ID |
| 1 | `timestamp` | `Utf8` | no | 来源时间戳的无损标量文本 |
| 2 | `filename` | `Utf8` | no | `/` 分隔的 source filename |
| 3 | `data` | `lance.json` / JSONB | no | 未 offload 时为完整 record；offload 时为 JSON null |
| 4… | 用户附加列 | `lance.json` / JSONB | yes | path 命中的 JSON value |
| N-1 | `_offload_` | `lance.json` / JSONB | yes | offload descriptor |
| N | `_raw_` | `LargeBinary` | yes | 未 offload 的 raw record |

JSON null 是非 null JSONB cell，不等于 Arrow null。`data` 因此始终是非 nullable JSONB。

每行 MUST 恰好满足以下一个状态：

1. inline：`_raw_` 非 null，`_offload_` 为 Arrow null，`data` 是完整 record；
2. offloaded：`_raw_` 为 Arrow null，`_offload_` 非 null，`data` 是 JSON null。

reader 遇到其他组合 MUST 拒绝导出。

## Offload

`offload_threshold = 0` 禁用 offload。否则 raw record 长度大于或等于阈值时 MUST offload。

descriptor 的 JSONB schema 为：

```json
{"path":"_offload","key":"<64 lowercase hexadecimal characters>"}
```

- `key` MUST 等于 raw record 的 BLAKE3-256 hex digest；
- object MUST 位于 `<dataset>/<path>/<key>`；
- 相同 key MUST 内容相同，可在多行间共享；
- import 发现已有 key 但内容不同 MUST 失败；
- export MUST 重新计算 digest，不匹配时 MUST 失败；
- reader MUST 拒绝绝对 `path` 或包含 `..` 的 `path`。

`id`、`timestamp`、`filename` 与用户附加列始终 inline，因此不读取 offload object 也可执行
这些列上的过滤和聚合。

## Import

```text
pchronicle import \
  --from ./jsonl-root \
  --to ./records.lance \
  --output-format compact-jsonl \
  --column id=$.event.id \
  --column timestamp=$.event.time \
  --column model=$.payload.model
```

`--input-format compact-jsonl` 与 `--output-format compact-jsonl` 任一均选择本格式。

- create MUST 拒绝已存在的目标；
- replace MUST 使用既有 `--yes` 确认规则；
- append MUST 被拒绝；
- 本地 CLI 发布 MUST 通过同父目录 staging 后原子替换；
- API `CompactJsonlStore::import_path` 表示完整快照写入，不提供 append 语义。

## Sync

```text
pchronicle sync \
  --from ./jsonl-root \
  --to ./warehouse-copy \
  --convert ./records.lance \
  --input-format compact-jsonl \
  --column id=$.event.id \
  --column timestamp=$.event.time
```

v1 sync 是 snapshot sync。每批变化 MUST 重新扫描完整 input root，并用一个完整的新 compact
snapshot 替换 `--convert`。创建、修改和删除源文件都必须反映到下一快照。v1 不承诺行级增量
更新。

## Export

```text
pchronicle export \
  --from ./records.lance \
  --to ./restored \
  --output-format compact-jsonl
```

export MUST：

1. 验证 `pchronicle.format=compact-jsonl/v1`；
2. 验证 `filename` 是相对路径且不含 `..`；
3. 按 Lance 物理行序，将每行写回 `filename`；
4. inline 行写 `_raw_`，offloaded 行验证并写 offload object；
5. 不添加、删除或规范化任何字节，包括空格、JSON key 顺序、数字表示和行结束符；
6. 默认拒绝已存在的输出，只有 `--overwrite` 可替换它；
7. 不支持 `--source`、ID filter 或 `--where`，因为过滤会破坏目录的无损镜像语义。

## 错误与安全边界

以下情况 MUST fail closed：格式 metadata 缺失或版本未知、schema 缺列或列类型错误、无效
JSON、无效 mapping、重复 ID、缺少必填标量、路径逃逸、offload 缺失或 digest 不匹配。

import/export 不跟随源目录中的符号链接。export 在创建任何文件前 SHOULD 验证 schema；每个
目标 filename 与 offload path MUST 在拼接前完成路径逃逸检查。

## 兼容性

reader MUST 只接受它明确支持的 `pchronicle.format`。v1 增加 nullable 用户列不改变基础列
语义；修改基础列类型、offload descriptor、逐字节恢复规则或 JSON path 语法均需要新的格式
版本。

Compact JSONL 是存储格式，不是 `DocumentFormat` trajectory codec。公共 Rust API 从
`persisting_pchronicle::storage` 暴露，具体实现归属 `store/compact_jsonl.rs`。
