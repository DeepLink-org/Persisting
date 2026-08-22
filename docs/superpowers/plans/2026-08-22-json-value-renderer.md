# JSON Value Renderer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn evidence 里的 JSON 值按形态派发：一层表、嵌套树（默认折叠）；字符串里的 JSON 对象/数组先 peel。

**Architecture:** `pchronicle-web/src/json_value.rs` 提供 `peel_json` / `classify_json` 和 `JsonValue`。`JsonValue` 先 peel 再派发到 `JsonScalar` / `JsonKvTable` / `JsonRecordTable` / `JsonTree`。表格单元格和树子节点回调 `JsonValue`。`EvidenceBlock` 改成标题 + children 宿主。

**Tech Stack:** Dioxus 0.7, `serde_json::Value`, Rust unit tests in `pchronicle-web` bin

## Global Constraints

- 只改 `pchronicle-web`；不改 Storyline / 后端 wire / agent pretty JSON
- 不识别 `{fn, args}` 等协议形状
- 不改查询单元格弹窗
- Reasoning 始终纯文本，不 peel
- 样式加在现有 `assets/components.css` / `assets/inline-trace.css`，`pc2-` 前缀
- 单测只覆盖 peel / classify / 列名 / 摘要；不要求 WASM 组件快照
- 规范：[`docs/superpowers/specs/2026-08-22-json-value-renderer-design.md`](../specs/2026-08-22-json-value-renderer-design.md)

---

## File Structure

| File | Responsibility |
|---|---|
| Create: `pchronicle-web/src/json_value.rs` | peel、classify、摘要、列名、`JsonValue` 与四个形态组件 |
| Modify: `pchronicle-web/src/main.rs` | `mod json_value` |
| Modify: `pchronicle-web/src/components.rs` | `EvidenceBlock` 宿主化；`InlineTurnDetail` 接入 |
| Modify: `pchronicle-web/assets/components.css` | `.pc2-json-*` 表与树 |
| Modify: `pchronicle-web/assets/inline-trace.css` | evidence 内 JSON 滚动高度；bump `index.html` 的 `?v=` |

---

### Task 1: peel / classify 纯函数

**Files:**
- Create: `pchronicle-web/src/json_value.rs`
- Modify: `pchronicle-web/src/main.rs`

**Interfaces:**
- Consumes: `serde_json::Value`
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum JsonShape { Scalar, KvTable, RecordTable, Tree }`
  - `pub fn peel_json(value: &Value) -> Value`
  - `pub fn classify_json(value: &Value) -> JsonShape`（先 `peel_json` 再分类；判断子字段是否标量时不 peel）
  - `pub fn is_structured_json(value: &Value) -> bool`（peel 后是 object 或 array）

- [ ] **Step 1: 声明模块并写失败测试**

在 `pchronicle-web/src/main.rs` 的 `mod` 列表中、`mod components;` 旁加入：

```rust
mod json_value;
```

创建 `pchronicle-web/src/json_value.rs`，先只放测试引用的空壳，让测试编译失败或断言失败：

```rust
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonShape {
    Scalar,
    KvTable,
    RecordTable,
    Tree,
}

pub fn peel_json(_value: &Value) -> Value {
    Value::Null
}

pub fn classify_json(_value: &Value) -> JsonShape {
    JsonShape::Scalar
}

pub fn is_structured_json(_value: &Value) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn peel_promotes_object_and_array_strings_only() {
        assert_eq!(peel_json(&json!({"a": 1})), json!({"a": 1}));
        assert_eq!(peel_json(&json!("{\"a\":1}")), json!({"a": 1}));
        assert_eq!(peel_json(&json!("[1,2]")), json!([1, 2]));
        assert_eq!(peel_json(&json!("not-json")), json!("not-json"));
        assert_eq!(peel_json(&json!("{")), json!("{"));
        assert_eq!(peel_json(&json!("\"hello\"")), json!("\"hello\""));
        assert_eq!(peel_json(&json!(7)), json!(7));
    }

    #[test]
    fn classify_matches_one_level_tables_and_trees() {
        assert_eq!(classify_json(&json!("plain")), JsonShape::Scalar);
        assert_eq!(classify_json(&json!({})), JsonShape::KvTable);
        assert_eq!(classify_json(&json!({"b": true, "a": 1})), JsonShape::KvTable);
        assert_eq!(
            classify_json(&json!([{"b": 2, "a": 1}, {"a": 3, "c": null}])),
            JsonShape::RecordTable
        );
        assert_eq!(
            classify_json(&json!([{"fn": "read", "args": "{\"path\":\"x\"}"}])),
            JsonShape::RecordTable
        );
        assert_eq!(
            classify_json(&json!("{\"fn\":\"read\",\"args\":\"{\\\"path\\\":\\\"x\\\"}\"}")),
            JsonShape::KvTable
        );
        assert_eq!(
            classify_json(&peel_json(&json!("{\"path\":\"x\"}"))),
            JsonShape::KvTable
        );
        assert_eq!(classify_json(&json!({"nested": {"x": 1}})), JsonShape::Tree);
        assert_eq!(classify_json(&json!([1, 2, 3])), JsonShape::Tree);
        assert_eq!(classify_json(&json!([])), JsonShape::Tree);
        assert_eq!(classify_json(&json!([{"a": 1}, "tail"])), JsonShape::Tree);
        assert_eq!(classify_json(&json!([{"a": {"b": 1}}])), JsonShape::Tree);
    }

    #[test]
    fn structured_detection_follows_peel() {
        assert!(!is_structured_json(&json!("hello")));
        assert!(is_structured_json(&json!({"a": 1})));
        assert!(is_structured_json(&json!("{\"a\":1}")));
        assert!(!is_structured_json(&json!("\"hello\"")));
    }
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --bin pchronicle-web -- json_value -- --test-threads=1
```

Expected: FAIL（`peel_promotes_object_and_array_strings_only` 断言 `Null != Object`，或同类）

- [ ] **Step 3: 实现 peel / classify**

把 `pchronicle-web/src/json_value.rs` 的三个函数换成：

```rust
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonShape {
    Scalar,
    KvTable,
    RecordTable,
    Tree,
}

pub fn peel_json(value: &Value) -> Value {
    match value {
        Value::String(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(parsed) if parsed.is_object() || parsed.is_array() => parsed,
            _ => value.clone(),
        },
        other => other.clone(),
    }
}

pub fn classify_json(value: &Value) -> JsonShape {
    classify_peeled(&peel_json(value))
}

pub fn is_structured_json(value: &Value) -> bool {
    let peeled = peel_json(value);
    peeled.is_object() || peeled.is_array()
}

fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn classify_peeled(value: &Value) -> JsonShape {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => JsonShape::Scalar,
        Value::Object(object) => {
            if object.values().all(is_scalar) {
                JsonShape::KvTable
            } else {
                JsonShape::Tree
            }
        }
        Value::Array(items) => {
            if !items.is_empty()
                && items.iter().all(|item| {
                    item.as_object()
                        .is_some_and(|object| object.values().all(is_scalar))
                })
            {
                JsonShape::RecordTable
            } else {
                JsonShape::Tree
            }
        }
    }
}
```

保留 Step 1 的 `#[cfg(test)]` 模块，不要删。

- [ ] **Step 4: 再跑测试，确认通过**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --bin pchronicle-web -- json_value -- --test-threads=1
```

Expected: `3 passed`

- [ ] **Step 5: Commit**

```bash
git add pchronicle-web/src/json_value.rs pchronicle-web/src/main.rs
git commit -m "$(cat <<'EOF'
Add JSON shape classification for turn evidence.

EOF
)"
```

---

### Task 2: 列名并集与树摘要

**Files:**
- Modify: `pchronicle-web/src/json_value.rs`

**Interfaces:**
- Consumes: Task 1 的 `peel_json`
- Produces:
  - `pub fn record_columns(rows: &[Value]) -> Vec<String>`（只收集 object key，首次出现顺序）
  - `pub fn json_summary(value: &Value) -> String`（先 peel；object → `{n keys}`；array → `[n items]`；string → `string`；number → `number`；bool → `boolean`；null → `null`）

- [ ] **Step 1: 写失败测试**

在 `pchronicle-web/src/json_value.rs` 的 `tests` 模块末尾追加：

```rust
    #[test]
    fn record_columns_keep_first_seen_union() {
        let rows = vec![
            json!({"b": 2, "a": 1}),
            json!({"a": 3, "c": null}),
        ];
        assert_eq!(record_columns(&rows), vec!["a", "b", "c"]);
    }

    #[test]
    fn json_summary_peels_and_names_types() {
        assert_eq!(json_summary(&json!({"a": 1, "b": 2})), "{2 keys}");
        assert_eq!(json_summary(&json!([1, 2, 3])), "[3 items]");
        assert_eq!(json_summary(&json!([])), "[0 items]");
        assert_eq!(json_summary(&json!("{}")), "{0 keys}");
        assert_eq!(json_summary(&json!("hello")), "string");
        assert_eq!(json_summary(&json!(true)), "boolean");
        assert_eq!(json_summary(&json!(1)), "number");
        assert_eq!(json_summary(&json!(null)), "null");
    }
```

- [ ] **Step 2: 跑测试，确认失败**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --bin pchronicle-web -- record_columns_keep_first_seen -- --test-threads=1
```

Expected: FAIL（`cannot find function record_columns`）

- [ ] **Step 3: 实现两个函数**

在 `classify_peeled` 之后、`#[cfg(test)]` 之前加入：

```rust
pub fn record_columns(rows: &[Value]) -> Vec<String> {
    let mut columns = Vec::new();
    for row in rows {
        if let Value::Object(object) = row {
            for key in object.keys() {
                if !columns.contains(key) {
                    columns.push(key.clone());
                }
            }
        }
    }
    columns
}

pub fn json_summary(value: &Value) -> String {
    match peel_json(value) {
        Value::Object(object) => format!("{{{} keys}}", object.len()),
        Value::Array(items) => format!("[{} items]", items.len()),
        Value::String(_) => "string".into(),
        Value::Number(_) => "number".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Null => "null".into(),
    }
}
```

`serde_json` 默认 `Map` 是 `BTreeMap`，单行 object 的 key 顺序是字典序，所以 `{"b":2,"a":1}` 的首次顺序是 `a` 然后 `b`。测试按这个写。

- [ ] **Step 4: 再跑测试**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --bin pchronicle-web -- json_value -- --test-threads=1
```

Expected: `5 passed`

- [ ] **Step 5: Commit**

```bash
git add pchronicle-web/src/json_value.rs
git commit -m "$(cat <<'EOF'
Add JSON table columns and tree summaries.

EOF
)"
```

---

### Task 3: `JsonValue` 派发组件与样式

**Files:**
- Modify: `pchronicle-web/src/json_value.rs`
- Modify: `pchronicle-web/assets/components.css`
- Modify: `pchronicle-web/assets/inline-trace.css`
- Modify: `pchronicle-web/index.html`

**Interfaces:**
- Consumes: `peel_json`, `classify_json` 的内部 `classify_peeled`（组件内先 `peel_json` 再 `match classify_json` 对 peeled 值渲染）、`record_columns`, `json_summary`
- Produces: `#[component] pub fn JsonValue(value: Value) -> Element`

- [ ] **Step 1: 实现四个形态 + 门面**

在 `pchronicle-web/src/json_value.rs` 顶部把 import 换成：

```rust
use dioxus::prelude::*;
use serde_json::Value;
```

在 `json_summary` 之后、`#[cfg(test)]` 之前加入：

```rust
fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

#[component]
pub fn JsonValue(value: Value) -> Element {
    let peeled = peel_json(&value);
    match classify_json(&value) {
        JsonShape::Scalar => rsx! { span { class: "pc2-json-scalar", "{scalar_text(&peeled)}" } },
        JsonShape::KvTable => rsx! { JsonKvTable { value: peeled } },
        JsonShape::RecordTable => rsx! { JsonRecordTable { value: peeled } },
        JsonShape::Tree => rsx! { JsonTree { value: peeled } },
    }
}

#[component]
fn JsonKvTable(value: Value) -> Element {
    let map = match value {
        Value::Object(map) => map,
        _ => return rsx! { span { class: "pc2-json-scalar", "—" } },
    };
    rsx! {
        table { class: "pc2-json-table pc2-json-kv",
            thead { tr { th { "key" } th { "value" } } }
            tbody {
                for (key, child) in map {
                    tr { key: "{key}",
                        th { scope: "row", "{key}" }
                        td { JsonValue { value: child } }
                    }
                }
            }
        }
    }
}

#[component]
fn JsonRecordTable(value: Value) -> Element {
    let rows = match value {
        Value::Array(rows) => rows,
        _ => return rsx! { span { class: "pc2-json-scalar", "—" } },
    };
    let columns = record_columns(&rows);
    rsx! {
        div { class: "pc2-json-scroll",
            table { class: "pc2-json-table pc2-json-records",
                thead { tr { for column in columns.iter() { th { "{column}" } } } }
                tbody {
                    for (row_index, row) in rows.iter().enumerate() {
                        tr { key: "{row_index}",
                            for column in columns.iter() {
                                td {
                                    JsonValue {
                                        value: match row {
                                            Value::Object(object) => {
                                                object.get(column).cloned().unwrap_or(Value::Null)
                                            }
                                            _ => Value::Null,
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn JsonTree(value: Value) -> Element {
    match value {
        Value::Object(map) if map.is_empty() => rsx! {
            details { class: "pc2-json-node", summary { span { class: "pc2-json-size", "{0 keys}" } } }
        },
        Value::Array(items) if items.is_empty() => rsx! {
            details { class: "pc2-json-node", summary { span { class: "pc2-json-size", "[0 items]" } } }
        },
        Value::Object(map) => rsx! {
            div { class: "pc2-json-tree",
                for (key, child) in map {
                    JsonTreeNode { key: "{key}", label: key, value: child }
                }
            }
        },
        Value::Array(items) => rsx! {
            div { class: "pc2-json-tree",
                for (index, child) in items.into_iter().enumerate() {
                    JsonTreeNode { key: "{index}", label: format!("[{index}]"), value: child }
                }
            }
        },
        other => rsx! { span { class: "pc2-json-scalar", "{scalar_text(&other)}" } },
    }
}

#[component]
fn JsonTreeNode(label: String, value: Value) -> Element {
    let summary = json_summary(&value);
    rsx! {
        details { class: "pc2-json-node",
            summary { span { class: "pc2-json-key", "{label}" } span { class: "pc2-json-size", "{summary}" } }
            JsonValue { value }
        }
    }
}
```

注意：`JsonTree` 里空对象分支的 `"{0 keys}"` 必须写成 `"{0 keys}"` 对应 `json_summary` 的 `{0 keys}`。空对象走 `classify` 是 **KvTable**，`JsonValue` 不会把空对象派到 `JsonTree`。空对象分支可删，只留空数组：

把 `JsonTree` 换成：

```rust
#[component]
fn JsonTree(value: Value) -> Element {
    match value {
        Value::Array(items) if items.is_empty() => rsx! {
            details { class: "pc2-json-node",
                summary { span { class: "pc2-json-size", "[0 items]" } }
            }
        },
        Value::Object(map) => rsx! {
            div { class: "pc2-json-tree",
                for (key, child) in map {
                    JsonTreeNode { key: "{key}", label: key, value: child }
                }
            }
        },
        Value::Array(items) => rsx! {
            div { class: "pc2-json-tree",
                for (index, child) in items.into_iter().enumerate() {
                    JsonTreeNode { key: "{index}", label: format!("[{index}]"), value: child }
                }
            }
        },
        other => rsx! { span { class: "pc2-json-scalar", "{scalar_text(&other)}" } },
    }
}
```

`<details>` 不要写 `open`，默认折叠。

- [ ] **Step 2: 加 CSS**

在 `pchronicle-web/assets/components.css` 文件末尾追加：

```css
.pc2-json-scroll {
  max-height: 360px;
  overflow: auto;
}

.pc2-json-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 11px;
}

.pc2-json-table th,
.pc2-json-table td {
  padding: 5px 8px;
  border: 1px solid #eef0f3;
  text-align: left;
  vertical-align: top;
}

.pc2-json-table thead th,
.pc2-json-kv th[scope="row"] {
  background: #f8fafc;
  color: #667085;
  font-size: 9px;
  font-weight: 700;
}

.pc2-json-scalar {
  white-space: pre-wrap;
  word-break: break-word;
  color: #344054;
}

.pc2-json-tree {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.pc2-json-node {
  border-left: 1px solid #e4e7ec;
  padding-left: 8px;
}

.pc2-json-node > summary {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  list-style: none;
}

.pc2-json-node > summary::-webkit-details-marker {
  display: none;
}

.pc2-json-key {
  color: #101828;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
}

.pc2-json-size {
  color: #667085;
  font-size: 9px;
}

.pc2-json-node > .pc2-json-table,
.pc2-json-node > .pc2-json-tree,
.pc2-json-node > .pc2-json-scroll,
.pc2-json-node > .pc2-json-scalar {
  margin: 6px 0 8px;
}
```

在 `pchronicle-web/assets/inline-trace.css` 的 `.pc2-inline-detail .pc2-evidence-block pre` 规则旁追加：

```css
.pc2-inline-detail .pc2-evidence-block .pc2-json-scroll,
.pc2-inline-detail .pc2-evidence-block .pc2-json-tree {
  max-height: 420px;
  overflow: auto;
}
```

把 `pchronicle-web/index.html` 里：

```html
<link rel="stylesheet" href="/assets/inline-trace.css?v=2">
<link rel="stylesheet" href="/assets/components.css?v=1">
```

改成：

```html
<link rel="stylesheet" href="/assets/inline-trace.css?v=3">
<link rel="stylesheet" href="/assets/components.css?v=2">
```

- [ ] **Step 3: 编译并跑 json_value 测试**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --bin pchronicle-web -- json_value -- --test-threads=1
```

Expected: `5 passed`；含 `JsonValue` 的 bin 能编过。

- [ ] **Step 4: Commit**

```bash
git add pchronicle-web/src/json_value.rs pchronicle-web/assets/components.css pchronicle-web/assets/inline-trace.css pchronicle-web/index.html
git commit -m "$(cat <<'EOF'
Render classified JSON as tables and collapsed trees.

EOF
)"
```

---

### Task 4: Turn evidence 接入

**Files:**
- Modify: `pchronicle-web/src/components.rs`

**Interfaces:**
- Consumes: `crate::json_value::{is_structured_json, JsonValue}`
- Produces: `EvidenceBlock { title: &'static str, open: bool, children: Element }`；`InlineTurnDetail` 按 spec 表接入

- [ ] **Step 1: 改 `EvidenceBlock` 为宿主**

在 `pchronicle-web/src/components.rs` 顶部 import 区增加：

```rust
use crate::json_value::{is_structured_json, JsonValue};
```

把现有

```rust
fn EvidenceBlock(title: &'static str, value: String) -> Element {
    rsx! { details { class: "pc2-evidence-block", open: title == "Message", summary { "{title}" } pre { "{value}" } } }
}
```

换成：

```rust
#[component]
fn EvidenceBlock(title: &'static str, #[props(default = false)] open: bool, children: Element) -> Element {
    rsx! { details { class: "pc2-evidence-block", open, summary { "{title}" } {children} } }
}
```

- [ ] **Step 2: 改 `InlineTurnDetail`**

把 `InlineTurnDetail` 整段换成（facts 行保持原样，只换块）：

```rust
#[component]
fn InlineTurnDetail(value: TurnDetail) -> Element {
    let message = value.turn.message.clone();
    let message_text = value.turn.text();
    let structured_message = is_structured_json(&message);
    let tool_calls = serde_json::to_value(&value.wire_tool_calls).unwrap_or(Value::Array(Vec::new()));
    let events = serde_json::to_value(&value.events).unwrap_or(Value::Array(Vec::new()));
    rsx! { div { class: "pc2-inline-detail-head", strong { "Full turn evidence" } }
        div { class: "pc2-inspector-facts", Fact { label: "Turn", value: format!("#{}", value.summary.id) } Fact { label: "Source", value: value.summary.source.clone() } Fact { label: "Kind", value: value.summary.kind.clone().unwrap_or_else(|| "unavailable".into()) } Fact { label: "Model", value: value.summary.model_name.clone().unwrap_or_else(|| "unavailable".into()) } Fact { label: "Latency", value: value.summary.latency_ms.map(format_ms).unwrap_or_else(|| "unavailable".into()) } Fact { label: "TTFT", value: value.summary.ttft_ms.map(format_ms).unwrap_or_else(|| "unavailable".into()) } Fact { label: "Tokens", value: value.summary.total_tokens.map(|tokens| tokens.to_string()).unwrap_or_else(|| "unavailable".into()) } Fact { label: "Token split", value: format!("{} in · {} out", optional_u64(value.summary.prompt_tokens), optional_u64(value.summary.completion_tokens)) } Fact { label: "Events", value: value.events.len().to_string() } }
        if structured_message {
            EvidenceBlock { title: "Message", open: true, JsonValue { value: message } }
        } else {
            EvidenceBlock { title: "Message", open: true, pre { "{message_text}" } }
        }
        if let Some(reasoning) = &value.turn.reasoning_content {
            EvidenceBlock { title: "Reasoning", pre { "{reasoning.clone()}" } }
        }
        if !value.wire_tool_calls.is_empty() {
            EvidenceBlock { title: "Tool calls", JsonValue { value: tool_calls } }
        }
        if let Some(observation) = value.turn.observation.clone() {
            EvidenceBlock { title: "Observation", JsonValue { value: observation } }
        }
        if !value.events.is_empty() {
            EvidenceBlock { title: "Raw linked events", JsonValue { value: events } }
        }
        if let Some(extra) = value.turn.extra.clone() {
            EvidenceBlock { title: "Extra", JsonValue { value: extra } }
        }
        if let Some(metrics) = value.turn.metrics.clone() {
            EvidenceBlock { title: "Metrics", JsonValue { value: metrics } }
        }
    }
}
```

`TurnDetail.turn` 已有 `extra: Option<Value>` 和 `metrics: Option<Value>`，不要改 `model.rs`。不要改 `agent.rs` 里给模型的 pretty JSON。不要改 `CellValue` / 查询弹窗。

- [ ] **Step 3: 编译测试**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --bin pchronicle-web -- --test-threads=1
```

Expected: 全部通过（含 `json_value` 与现有 `components` / `chat_view` / `model` 测试）。

本地预览（需要时）：

```bash
just chronicle-web-build
```

然后用已有 `pchronicle serve` 看一条带 tool calls / observation 的 turn：扁平对象应是表，嵌套应是默认折叠的树，JSON 字符串格子里再派发。

- [ ] **Step 4: Commit**

```bash
git add pchronicle-web/src/components.rs
git commit -m "$(cat <<'EOF'
Host turn evidence JSON through JsonValue.

EOF
)"
```

---

## Spec coverage

| Spec | Task |
|---|---|
| peel 只处理当前节点；子字段不 peel | 1 |
| Scalar / KvTable / RecordTable / Tree 判定 | 1 |
| 空对象 KvTable；空数组 Tree | 1 |
| 非法 JSON / JSON 标量字符串 → Scalar | 1 |
| RecordTable 列并集、首次出现 | 2 |
| 树摘要 `{n keys}` / `[n items]` | 2 |
| `JsonValue` 派发四形态；单元格/子节点回调 | 3 |
| `<details>` 默认折叠 | 3 |
| `pc2-` 样式、不新开 CSS 管线 | 3 |
| EvidenceBlock 宿主；Message / Reasoning / 各 JSON 块 / Extra / Metrics | 4 |
| 不改查询单元格、不改 agent pretty JSON | 4（明确不碰） |
