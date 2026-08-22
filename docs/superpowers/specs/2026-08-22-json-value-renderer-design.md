# Turn evidence：按类型派发的 JSON 值渲染

## Status

Approved in conversation on 2026-08-22: approach 2 (classifier + small
components behind `JsonValue`); first host is Turn evidence.

## Context

Turn evidence 把 Tool calls、Observation、Raw events，以及非字符串
`message`，一律 `serde_json::to_string_pretty` 丢进 `<pre>`。扁平对象和深层
树看起来一样，工具 `arguments` 经常还是 JSON 字符串。

查询单元格弹窗也是 `<pre>`，本轮不改。

## Goals

1. 通用 `JsonValue`：按 JSON 形态派发，不按 evidence 块名特判。
2. 一层扁平对象 → 两列表；一层扁平对象数组 → 多列表。
3. 更深的对象/数组 → `<details>` 树，默认全关，打开只露一层。
4. 当前节点若是能解成对象/数组的字符串，先 peel 再分类。
5. 接到现有 Turn evidence；`extra` / `metrics` 有值才出块。

## Non-goals

- 查询结果单元格 / 单元格弹窗。
- 识别 `{fn, args}` 等协议形状。
- 改 agent 送给模型的 pretty JSON。
- 改 Reasoning 的纯文本语义（不 peel）。
- 重编嵌入前端以外的后端 wire。

## Decision

### 模块

`pchronicle-web/src/json_value.rs` 只暴露：

- `peel_json(&Value) -> Value`
- `classify_json(&Value) -> JsonShape`
- `JsonValue` 组件

`JsonShape`：`Scalar` | `KvTable` | `RecordTable` | `Tree`。
四个小组件各管一种形态。表格单元格和树子节点只回调 `JsonValue`。

`components.rs` 的 `EvidenceBlock` 改成宿主（`title` + 子内容），不再收
pretty 字符串。

### Peel

只处理**当前**节点：

- 字符串 `from_str` 得到对象或数组 → 用解析结果。
- 失败、得到标量、非字符串 → 原值。
- 不递归 peel 子字段。因此 `[{fn, args:"{...}"}]` 仍是一层 RecordTable，
  `args` 格子里再派发成表或树。

`peel` 最多跟节点走，不设跨节点深度预算；循环嵌套 JSON 字符串由下一层
`JsonValue` 再 peel。

### 分类（对 peel 后的当前值；判断子字段是否标量时不 peel）

标量：`null` / bool / number / string。

| 形态 | 条件 |
|---|---|
| Scalar | 标量 |
| KvTable | 对象（含空对象），且每个值都是标量 |
| RecordTable | 非空数组，每项都是对象，且每个对象的每个字段都是标量 |
| Tree | 其余对象或数组（含空数组、标量数组、混杂项、含嵌套的对象） |

空对象：0 行 KvTable。空数组：折叠树，摘要 `[0 items]`。

### 渲染

- Scalar：纯文本（null / bool / number 用 `Display`；字符串原文）。
- KvTable：两列 `key` / `value`；value 格是 `JsonValue`。
- RecordTable：列名取对象 key 并集，稳定顺序（首次出现）；单元格是 `JsonValue`。
- Tree：每个子项一个 `<details>`，默认 `open=false`。摘要：`key` 或 `[i]` +
  类型规模（`{3 keys}` / `[12 items]`），不铺整段 JSON。展开后子项再走
  `JsonValue`。

### Turn evidence 接入

`InlineTurnDetail`：

| 块 | 规则 |
|---|---|
| Message | peel 后仍是标量 → 现有正文；对象/数组 → `JsonValue` |
| Reasoning | 始终纯文本，不 peel |
| Tool calls | `wire_tool_calls` 序列化成 `Value` 后 `JsonValue` |
| Observation | 有值则 `JsonValue` |
| Raw linked events | 非空则 `JsonValue` |
| Extra / Metrics | `turn.extra` / `turn.metrics` 有值才出块，走 `JsonValue` |

块外壳仍是 `<details class="pc2-evidence-block">`；Message 默认展开，其余
默认折叠。块内不再用整段 `<pre>` 作为主渲染。

### 样式

沿用 `pc2-` 前缀，加在现有 `assets/components.css` / `assets/inline-trace.css`。
不新开 CSS 管线。树用原生 `<details>`。

## Files

- `pchronicle-web/src/json_value.rs` — peel / classify / 四个形态 / `JsonValue`
- `pchronicle-web/src/components.rs` — `EvidenceBlock` 宿主化；`InlineTurnDetail` 接入
- `pchronicle-web/src/main.rs` — `mod json_value`
- `pchronicle-web/assets/components.css`、`assets/inline-trace.css` — 表与树样式

## Test

单测 `peel_json` / `classify_json`：

- 扁平对象 → KvTable
- 扁平对象数组 → RecordTable
- 对象数组但 `args` 是 JSON 字符串 → 仍 RecordTable（子字段不 peel）
- 该字符串 peel 后 → KvTable 或 Tree
- 含嵌套对象 → Tree
- 标量数组、空数组 → Tree
- 空对象 → KvTable
- 非法 JSON 字符串、JSON 编码的标量字符串 → Scalar

不要求 WASM 组件快照。
