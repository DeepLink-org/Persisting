# RFC-0001: Storyline Format

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Format** | `storyline` |
| **Date** | 2026-07-30 |
| **Component** | pChronicle (`persisting-pchronicle`) |
| **Implements** | `crates/persisting-pchronicle/src/formats/storyline.rs` |
| **Related** | [RFC-0002 Events](0002-events-format.md) · [RFC-0004 ACTF](0004-actf-format.md) · [RFC-0008 ATIF](0008-atif-format.md) · [RFC-0009 OpenAI Messages](0009-openai-messages-format.md) |

---

## 摘要

**Storyline** 是 pChronicle 的**枢纽 interchange**：以 Harbor **ATIF-v1.7** 的 Trajectory / Step 折叠语义为基准，加上少量 hub 便利字段（短名 wire、性能顶栏、可选子会话外链）。

捕获侧：`HTTP → events 流`（可 **记录** 到 `events.lance`，也可 **触发** handler）；handler 经 storyline 做格式转换与落盘。外围格式（`agenticmd` / `openai_msg` / `atif` / `actf`）**只与 storyline 互转**。

Storyline **不是**事实源。Canonical 事实仍是 `events.lance`。

```text
events ──┐
agenticmd ┼──► storyline ──► …
openai_msg┤
atif ─────┘
```

---

## 设计原则

1. **ATIF-first**：根对象 ≈ Trajectory，`turns[]` ≈ `steps[]`；能 1:1 的字段保持同义。
2. **Hub-only**：A→B MUST 经 storyline。
3. **少叙事**：不引入 Capture Call / 相位 / event 回链等运行时读模型；那些留在 events。
4. **Not SoT**：回放与审计以 events 为准。

相对 ATIF 仅保留这些增量：

| 增量 | 说明 |
|---|---|
| 短名 wire（`src`/`msg`/`ts`/…） | JSON 更短；长名仅作字段概念说明 |
| `latency_ms` / `ttft_ms` | 常从 `metrics` 提升到 turn 顶栏 |
| `tool_calls[].duration_ms` | 常从 tool_call.`extra` 提升 |
| `agent.id` | ATIF 仅有 `name` 时 `id = name` |
| `session` Required | ATIF `session_id` 在 v1.7 可为可选 |
| 可选 `parent` / `children` | ATIF `subagent_trajectories` 的外链表达（默认不内嵌整树） |
| Required `schema_version` | 当前只接受 `storyline/v1`；未知版本 fail closed |
| 可选 `origin` | 记录来源格式、来源 schema 与文档身份，不冒充 Storyline 自身版本 |
| `unknown_fields` | 仅保存 Storyline 不认识的源格式 key/value，按来源和 JSON Pointer 隔离 |

---

## 格式映射的权威边界

本 RFC 只定义 Storyline wire schema。外围格式到 Storyline 的字段映射由对应格式 RFC
负责，设计文档和实现说明不得另行维护一份竞争性映射表：

- [RFC-0004 § ACTF → Storyline JSON Pointer 映射](0004-actf-format.md#actf-storyline-json-pointer-mapping)
- [RFC-0008 § ATIF → Storyline JSON Pointer 映射](0008-atif-format.md#atif-storyline-json-pointer-mapping)
- [RFC-0009 § OpenAI Messages → Storyline JSON Pointer 映射](0009-openai-messages-format.md#openai-storyline-json-pointer-mapping)

同源恢复使用 `unknown_fields` 保留 Storyline 不认识的源字段。跨外围格式转换只保证输出
目标格式能够表达的 Storyline 语义；不能由目标格式表达的外来 residual 使用受控
`_storyline` envelope 携带。

---

## Wire schema

编码：UTF-8 JSON。根对象 MUST 包含 `schema_version: "storyline/v1"`、`session`、
`agent` 和 `turns`。所有拥有字段的对象都拒绝未声明 key；扩展只能进入显式
`extra` 或受限 `unknown_fields`。

### Wire 短名

JSON 序列化和解码都使用短名；长名仅用于说明字段概念，不作为兼容输入。

| 短名 | 字段概念 | 位置 |
|---|---|---|
| `run` | `run_id` | root |
| `trajectory` | `trajectory_id` | root |
| `session` | `session_id` / `story_id` | root |
| `children` | `child_session_ids` / `child_story_ids` | root |
| `ver` | `version` | agent |
| `model` | `model_name` | agent / turn |
| `tools` | `tool_definitions` | agent |
| `psid` | `parent_session_id` / `parent_story_id` | parent |
| `scid` | `spawn_call_id` | parent |
| `ptid` | `spawn_id` | parent |
| `rel` | `relation` | parent |
| `ts` | `timestamp` | turn |
| `src` | `source` | turn |
| `msg` | `message` | turn |
| `reason` | `reasoning_content` | turn |
| `effort` | `reasoning_effort` | turn |
| `nllm` | `llm_call_count` | turn |
| `copied` | `is_copied_context` | turn |
| `tcid` | `tool_call_id` | tool_calls[] |
| `fn` | `function_name` | tool_calls[] |
| `args` | `arguments` | tool_calls[] |

保持全名：`agent` / `final_metrics` / `continued_trajectory_ref` / `tool_calls` / `observation` / `metrics` / `extra` / `latency_ms` / `ttft_ms` / `duration_ms`，以及 `id` / `name` / `notes` / `kind` / `turns` / `parent`。

### 根对象

| Wire | Type | Status |
|---|---|---|
| `schema_version` | string | Required，固定 `storyline/v1` |
| `origin` | object | Optional；来源格式、schema 与文档身份 |
| `session` | string | Required；非空 |
| `agent` | object | Required |
| `turns` | array | Required |
| `run` | string | Optional；run-scoped identity |
| `trajectory` | string | Optional；非空 |
| `attempt_id` | string | Optional；源格式 attempt identity |
| `notes` | string | Optional |
| `final_metrics` | any | Optional |
| `continued_trajectory_ref` | string | Optional |
| `extra` | any | Optional；Storyline 业务扩展 |
| `unknown_fields` | object | Optional；源格式 residual，见下 |
| `unknown_key_counts` | object | Optional；必须与 `unknown_fields` 一致 |
| `children` | string[] | Optional；子 Storyline identity 外链 |
| `parent` | object | Optional；父 Storyline 外链 |

### `origin`

| Wire | Type | Status |
|---|---|---|
| `format` | string | Required；非空 |
| `schema_version` | string | Optional；非空 |
| `document_id` | string | Optional；非空 |

### `agent`

| Wire | Type | Status |
|---|---|---|
| `id` | string | Required；非空 |
| `name` | string | Optional |
| `ver` | string | Optional |
| `model` | string | Optional |
| `tools` | any | Optional |
| `extra` | any | Optional |

### `parent`（可选外链）

对应 ATIF 内嵌 `subagent_trajectories` 的轻量替代；**不是**必填叙事层。

| Wire | Type | Status | Description |
|---|---|---|---|
| `psid` | string | Required | 父 `session` |
| `scid` | string | Optional | 父侧触发 id（若有） |
| `ptid` | integer | Optional | 父侧 turn `id` |
| `rel` | string | Optional | 默认 `"spawn"` |

完整子轨迹 SHOULD 作独立 `storyline.json`；导出单文件 ATIF 时转换器 MAY 再内嵌。

### `turns[]`

| Wire | Type | Status |
|---|---|---|
| `id` | integer | Required；文档内唯一 |
| `src` | string | Required；非空 |
| `msg` | any | Required |
| `ts` | string \| number | Optional；RFC3339 或可精确表示为纳秒的 Unix epoch 秒数 |
| `model` | string | Optional |
| `effort` | any | Optional |
| `reason` | string | Optional |
| `tool_calls` | array | Optional |
| `observation` | any | Optional |
| `metrics` | any | Optional |
| `nllm` | integer | Optional |
| `copied` | boolean | Optional |
| `extra` | any | Optional |
| `kind` | string | Optional；省略时由 `src` 与 `tool_calls` 推导 |
| `latency_ms` | integer | Optional |
| `ttft_ms` | integer | Optional |

### `tool_calls[]`

| Wire | Type | Status |
|---|---|---|
| `tcid` | string | Required；全文档唯一且非空 |
| `fn` | string | Required；非空 |
| `args` | any | Required |
| `result` | any | Optional |
| `duration_ms` | integer | Optional |
| `extra` | any | Optional |

### `unknown_fields` 与 `unknown_key_counts`

`unknown_fields` 只保存外围格式中无法映射到 Storyline 已知字段的值：

```text
/unknown_fields/sources/{source}/source_document_id = string
/unknown_fields/sources/{source}/fields/{E(P)}       = any
```

`P` 是源文档中的完整 RFC 6901 JSON Pointer；`E(P)` 是把整个 `P` 作为 `fields`
对象 key 后执行一次 RFC 6901 token 转义。`unknown_key_counts` 按 source 和规范化 pointer
记录出现次数，必须能由 `unknown_fields` 确定性重算。已知业务扩展写入 `extra`，不得混入
`unknown_fields`。

---

## 校验（Normative）

MUST 拒绝：缺失或无法识别的 `schema_version`；空 `session` / 空 `agent.id`；重复的
`turns[].id`；全局重复或空的 `tool_calls[].tcid`；空 `fn`；既非 RFC3339 字符串、也非
可精确表示为纳秒的 Unix epoch 秒数的 `ts`；未声明的
拥有字段；不匹配的 `unknown_key_counts`。

`turns` 数组顺序具有语义，MUST 原样保留；`id` 只用于身份和关联，不用于排序。
导出 ATIF 时 `step_id = turns[].id`；未知 `extra` 透传。

---

## 枢纽 API

```text
into_storyline(format, input)  → StorylineDocument
from_storyline(format, story)  → serialized
convert(from, to, input)       ≡ from_storyline(to, into_storyline(from, input))
```

`events` 为 Lance-only：字符串 convert API 对 `ChronicleFormat::Events` 报错；内存用 `events_to_storyline` / `storyline_to_events`。

---

## 文件与探测

| 约定 | 值 |
|---|---|
| 文件名 | `storyline.json` / `{session}.storyline.json` |
| 内容 | `schema_version: "storyline/v1"` + `session` + `turns` |

实现：`into_storyline` / `from_storyline` / `convert`。

---

## History

| Date | Notes |
|---|---|
| 2026-07-30 | 初稿与迭代（hub、短名、去 `calls[]`、性能字段、`session`） |
| 2026-07-30 | 收敛为 ATIF-first：去掉 Capture Call/Normal 过度叙事；`continued_trajectory_ref` 对齐 ATIF；`parent.scid` 可选 |
| 2026-08-20 | 固定 `storyline/v1`，增加 `origin` 与统一 unknown fields；Lance v2 保留数组顺序、出现语义和原始 observation |
| 2026-08-21 | Storyline schema 与外围格式映射分离；映射由各格式 RFC 独立负责 |
