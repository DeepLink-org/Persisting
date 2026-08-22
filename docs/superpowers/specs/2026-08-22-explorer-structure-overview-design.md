# Explorer Trace：Structure 结构芯片 + Overview 摘要

## Status

Approved in conversation on 2026-08-22: Structure holds type chips and
counts; Overview is the extracted utterance; list API adds `char_count`
and `modalities`, and `preview` becomes extracted text.

## Context

Chats / Steps 共用原来的 span 表。Structure 几乎只有 `Chat 5` 和
`user → agent · seq 4–5`。Overview 把 `turn.message` 原样
`to_string` 再截断，多模态数组先露出 `image_bytes:null` 一类外壳，
真正的 `text` 被挤掉。模型名、耗时、SYSTEM/CHAT 徽章和 Structure /
Evidence 重复。

扫 60 行时，两列要分工：左边回答「这是什么、多大」，右边回答「说了什么」。

## Goals

1. Structure 展示结构化信息：大类型 + 细节形态芯片 + 组成 + 字符数 + seq。
2. Overview 只展示抽出的对话摘要。
3. 列表 `preview` 改为抽出正文，过滤 / 搜索 / Copilot 能命中原话。
4. 形态芯片可靠，不依赖 180 字截断后的 JSON 残片。

## Non-goals

- 改 Storyline / ATIF / ACTF 映射或 Lance 投影。
- 改 Sequence 轴（会话相对位置 + 按 turn 类型上色）。
- 改 Evidence 列。
- 为列表再拉 turn 详情。
- 用模型生成摘要；摘要就是抽出的正文截断。
- 在 Overview 保留模型名、耗时、角色大徽章。

## Decision

### 两列分工

| 列 | 放什么 | 不放什么 |
|---|---|---|
| Structure | 大类型芯片、形态芯片、组成、字符数、seq、行标题 | 正文、模型、耗时 |
| Overview | 抽出的摘要，单行截断，`title` 为同一段 `preview` | 类型芯片、JSON 外壳 |

去掉 Structure 左侧色点和 Overview 里的 SYSTEM / USER / CHAT 大徽章。
大类型只留一枚芯片，紧挨行标题（`Chat 5` / `System` / `#8`）。

**Structure 一格顺序**

1. 行标题 + 大类型芯片
2. 细节芯片（没有的不画）
3. 一行小字：`1 user + 1 agent · 1 tool · 842 chars · seq 4–5`

### 大类型

| 行 | 芯片 |
|---|---|
| Chats：`TraceCard::Chat`（含无 user 的前导 agent） | `Chat` |
| Chats：`TraceCard::System` | `System` |
| Steps 行 | `User` / `Agent` / `System`（`source`） |
| 根行 | 无大类型芯片；标题仍是 `trajectory · N chats\|steps` |

展开后的 `CompactTurnRow` 不复制整套 Structure 芯片；角色仍用现有
`pc2-role`，正文改用抽出的 `preview`。

### 细节形态

稳定顺序：`text` · `image` · `audio` · `tool_call`。只画实际出现的。

Chat 行 / 根行：成员 `modalities` 并集。单条 turn：自己的列表。

### 组成与长度

- 组成：按 `source` 计数，工具数用 `tool_names.len()` 之和。
  例：`1 user + 1 agent · 1 tool`；只有 system：`1 system`；
  无 user 的 agent 行：`1 agent · 1 tool`。零工具时省略 `· 0 tool`。
- 长度：抽出正文的**完整**字符数（截断前），展示为 `842 chars` 或
  `1.2k chars`（≥1000 用一位小数的 `k`）。
- **Chat 行的 `chars` 只计用户摘要**。没有 user → `0 chars`，Overview
  为 `No user turn`。
- 根行：各源计数 + 全会话形态并集 + **用户摘要字符合计**。Overview
  不编摘要。

### 摘要

- Chat 行：第一条 user 的 `preview`。没有 user → `No user turn`。
- Steps 行：该 turn 的 `preview`。空 → `No text`。
- 展开的 `CompactTurnRow`：同样用该 turn 的 `preview` / `No text`。
- 单行截断；`title` 等于 `preview`（服务端已截到约 180 字）。全文在
  turn 详情，不把未截断 message 放进列表。

### 列表 wire

`TurnSummary`（CLI explorer 与 `pchronicle-web` 对齐）在现有字段上：

- `preview`：**抽出的可读正文**，再 `compact` 到约 180 字。不再
  `serde_json::to_string(message)`。
- `char_count: u64`：抽出正文截断前的字符数。
- `modalities: Vec<String>`：`text` / `image` / `audio` / `tool_call`。

旧客户端缺字段：前端当 `preview` 空、`char_count = 0`、无形态，不报错。
不改 Storyline schema。搜索 / 过滤继续用 `preview` 和 `source`。

### 从 `turn.message` 抽出

服务端在 `turn_summary` 里算，前端只展示和按行聚合。

**正文**

- 字符串 message → 整段。
- 数组 / 对象：递归走进子节点，收集非空字符串 `text`，以及本身是
  字符串的 `content`。多段用空格拼接。`null` / 空串不算。
- 抽不出 → `preview` 空，`char_count = 0`。

**形态（有才记）**

- `text`：抽出正文非空。
- `image`：`image` / `image_url` / `image_bytes` 有非空值，或 part
  `type` 为 `image` / `image_url`。
- `audio`：`input_audio` / `audio` 有非空值，或 `type` 为 `audio` /
  `input_audio`。
- `tool_call`：`display_tool_calls` 非空，或正文含 `<tool_call>`。

空字符串、`null`、缺省 key 都不构成形态。

### 文件

- `crates/persisting-pchronicle-cli/src/server/explorer.rs` — 抽出、
  `TurnSummary` 新字段、单测
- `pchronicle-web/src/model.rs` — `TurnSummary` 对齐
- `pchronicle-web/src/components.rs` — Structure / Overview 渲染与聚合
- `pchronicle-web/assets/span-timeline.css` — 芯片样式
- 现有 `chat_view` 分组与过滤规则不动

## Test

服务端（`explorer`）：

- 多模态数组：前部 `null` 媒体字段 + 末尾 `text` → 正文是那句 text，
  `char_count` 为完整长度，`modalities` 含 `text`，不含空 image/audio
- 纯字符串 message → 正文即字符串，`text`
- 只有非空 `image_url`、无 text → 空 preview，`0`，`[image]`
- 有 `tool_names` 或 `<tool_call>` → 含 `tool_call`

前端（`pchronicle-web`）：

- user + agent Chat：形态并集；`chars` 等于 user `char_count`；
  Overview 是 user `preview`
- 无 user 的 Chat：Overview `No user turn`，`0 chars`，大类型仍是
  `Chat`
- Steps 空 preview → `No text`

不要求 e2e。
