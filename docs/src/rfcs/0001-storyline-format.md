# RFC-0001: Storyline Format

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Format** | `storyline` |
| **Date** | 2026-07-30 |
| **Component** | pChronicle (`persisting-pchronicle`) |
| **Implements** | `crates/persisting-pchronicle/src/formats/storyline.rs` |
| **Related** | [Harbor ATIF](https://github.com/harbor-framework/harbor/blob/main/rfcs/0001-trajectory-format.md) · [RFC-0002 Events](0002-events-format.md) |

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
| 短名 wire（`src`/`msg`/`ts`/…） | JSON 更短；长名作 decode alias |
| `latency_ms` / `ttft_ms` | 常从 `metrics` 提升到 turn 顶栏 |
| `tool_calls[].duration_ms` | 常从 tool_call.`extra` 提升 |
| `agent.id` | ATIF 仅有 `name` 时 `id = name` |
| `session` Required | ATIF `session_id` 在 v1.7 可为可选 |
| 可选 `parent` / `children` | ATIF `subagent_trajectories` 的外链表达（默认不内嵌整树） |

---

## 与 ATIF 的对照

参照 [Harbor ATIF](https://github.com/harbor-framework/harbor/blob/main/rfcs/0001-trajectory-format.md)（**ATIF-v1.7**）。ATIF 在 pChronicle 中是外围格式；Storyline 是 hub。

```text
ATIF:      Trajectory  →  steps[]  →  tool_calls / observation / metrics
Storyline: Document    →  turns[]  →  tool_calls / observation / metrics (+ latency_*)
```

### 根 / Agent

| ATIF | Storyline wire | 说明 |
|---|---|---|
| `session_id` | `session` | 同义；Storyline Required |
| `trajectory_id` | `trajectory` | 文档级身份；与 run-scoped `session_id` 分离 |
| `agent.name` / `version` / `model_name` / `tool_definitions` / `extra` | `agent.name` / `ver` / `model` / `tools` / `extra` | 另有 Required `agent.id` |
| `notes` / `final_metrics` / `extra` | 同名（全名） | |
| `continued_trajectory_ref` | `continued_trajectory_ref` | 同名 |
| `subagent_trajectories[]` | `children`（+ 可选 `parent`） | 默认外链；导出 ATIF MAY 再内嵌 |

### Step ↔ Turn

| ATIF `StepObject` | Storyline turn | 说明 |
|---|---|---|
| `step_id` | `id` | 1:1（推荐 1-based） |
| `source` | `src` | `user` \| `agent` \| `system` |
| `message` | `msg` | 唯一正文；user / agent **各占一轮** |
| `timestamp` | `ts` | |
| `model_name` | `model` | |
| `reasoning_effort` / `reasoning_content` | `effort` / `reason` | |
| `tool_calls` / `observation` / `metrics` / `extra` | 同名（全名） | |
| `llm_call_count` / `is_copied_context` | `nllm` / `copied` | |
| （常在 metrics） | `latency_ms` / `ttft_ms` | 顶栏便利字段 |

### ToolCall

| ATIF | Storyline wire |
|---|---|
| `tool_call_id` | `tcid` |
| `function_name` | `fn` |
| `arguments` | `args` |
| `extra`（可含 `duration_ms`） | `extra` + 可选顶栏 `duration_ms` |

观察结果仍按 ATIF：`observation.results[].source_call_id` ↔ `tool_call_id`。

### 互转保真

| 方向 | 目标 |
|---|---|
| ATIF ↔ storyline | JSON 数据模型级无损；保留三态、嵌套 subagent、身份和 RFC3339 原文 |
| ACTF/OpenAI Msg ↔ storyline | 同源恢复使用受控 residual，保证 JSON 数据模型级无损 |
| 跨外围格式 | 输出目标格式可表达的全部语义；目标无对应字段时显式使用合成 API |

---

## Wire schema

编码：UTF-8 JSON。根对象 MUST 包含 `session`、`agent` 和 `turns`。

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

| Wire | Type | Status | ATIF |
|---|---|---|---|
| `session` | string | Required | `session_id` |
| `agent` | object | Required | `agent` |
| `turns` | array | Required | `steps` |
| `run` | string | Optional | `trajectory_id` |
| `notes` | string | Optional | `notes` |
| `final_metrics` | object | Optional | `final_metrics` |
| `continued_trajectory_ref` | string | Optional | `continued_trajectory_ref` |
| `extra` | object | Optional | `extra` |
| `children` | string[] | Optional | （外链索引，对应子 session / trajectory id） |
| `parent` | object | Optional | （外链；见下） |

### `agent`

| Wire | Type | Status | ATIF |
|---|---|---|---|
| `id` | string | Required | （无独立字段；缺省 = `name`） |
| `name` | string | Optional | `name` |
| `ver` | string | Optional | `version` |
| `model` | string | Optional | `model_name` |
| `tools` | any | Optional | `tool_definitions` |
| `extra` | object | Optional | `extra` |

### `parent`（可选外链）

对应 ATIF 内嵌 `subagent_trajectories` 的轻量替代；**不是**必填叙事层。

| Wire | Type | Status | Description |
|---|---|---|---|
| `psid` | string | Required | 父 `session` |
| `scid` | string | Optional | 父侧触发 id（若有） |
| `ptid` | integer | Optional | 父侧 turn `id` |
| `rel` | string | Optional | 默认 `"spawn"` |

完整子轨迹 SHOULD 作独立 `storyline.json`；导出单文件 ATIF 时转换器 MAY 再内嵌。

### `turns[]`（≈ ATIF Step）

| Wire | Type | Status | ATIF |
|---|---|---|---|
| `id` | integer | Required | `step_id` |
| `src` | string | Required | `source` |
| `msg` | string \| array | Required | `message` |
| `ts` | string | Optional | `timestamp` |
| `model` | string | Optional | `model_name` |
| `effort` | string \| number | Optional | `reasoning_effort` |
| `reason` | string | Optional | `reasoning_content` |
| `tool_calls` | array | Optional | `tool_calls` |
| `observation` | object | Optional | `observation` |
| `metrics` | object | Optional | `metrics` |
| `nllm` | integer | Optional | `llm_call_count` |
| `copied` | boolean | Optional | `is_copied_context` |
| `extra` | object | Optional | `extra` |
| `kind` | string | Optional | —（可省略；由 `src`+`tool_calls` 推导） |
| `latency_ms` | integer | Optional | 常来自 `metrics` |
| `ttft_ms` | integer | Optional | 常来自 `metrics` |

### `tool_calls[]`

| Wire | Type | Status | ATIF |
|---|---|---|---|
| `tcid` | string | Required | `tool_call_id` |
| `fn` | string | Required | `function_name` |
| `args` | any | Required | `arguments` |
| `duration_ms` | integer | Optional | `extra.duration_ms` |
| `extra` | object | Optional | `extra` |

---

## 校验（Normative）

MUST 拒绝：空 `session` / 空 `agent.id`；重复的 `turns[].id`；无法识别的 `storyline/vN`。

SHOULD：`turns` 按 `id` 升序；导出 ATIF 时 `step_id = turns[].id`；未知 `extra` 透传。

---

## 枢纽 API

```text
into_storyline(format, input)  → StorylineDocument
from_storyline(format, story)  → serialized
convert(from, to, input)       ≡ from_storyline(to, into_storyline(from, input))
```

`events` 为 Lance-only：字符串 convert API 对 `ChronicleFormat::Events` 报错；内存用 `events_to_storyline` / `storyline_to_events`。

---

## 示例（可执行抽取映射）

value = 在 ATIF 根上求值的 JSONPath。

```json
{
  "run": "$.trajectory_id",
  "session": "$.session_id",
  "agent": {
    "id": "$.agent.name",
    "name": "$.agent.name",
    "ver": "$.agent.version",
    "model": "$.agent.model_name",
    "tools": "$.agent.tool_definitions",
    "extra": "$.agent.extra"
  },
  "children": "$.subagent_trajectories[*].trajectory_id",
  "notes": "$.notes",
  "final_metrics": "$.final_metrics",
  "continued_trajectory_ref": "$.continued_trajectory_ref",
  "extra": "$.extra",
  "turns": [
    {
      "id": "$.steps[0].step_id",
      "src": "$.steps[0].source",
      "msg": "$.steps[0].message",
      "ts": "$.steps[0].timestamp",
      "model": "$.steps[0].model_name",
      "effort": "$.steps[0].reasoning_effort",
      "reason": "$.steps[0].reasoning_content",
      "tool_calls": "$.steps[0].tool_calls",
      "observation": "$.steps[0].observation",
      "metrics": "$.steps[0].metrics",
      "nllm": "$.steps[0].llm_call_count",
      "copied": "$.steps[0].is_copied_context",
      "extra": "$.steps[0].extra"
    },
    {
      "id": "$.steps[1].step_id",
      "src": "$.steps[1].source",
      "msg": "$.steps[1].message",
      "ts": "$.steps[1].timestamp",
      "model": "$.steps[1].model_name",
      "effort": "$.steps[1].reasoning_effort",
      "reason": "$.steps[1].reasoning_content",
      "tool_calls": "$.steps[1].tool_calls",
      "observation": "$.steps[1].observation",
      "metrics": "$.steps[1].metrics",
      "nllm": "$.steps[1].llm_call_count",
      "copied": "$.steps[1].is_copied_context",
      "extra": "$.steps[1].extra",
      "latency_ms": "$.steps[1].metrics.latency_ms",
      "ttft_ms": "$.steps[1].metrics.ttft_ms"
    }
  ]
}
```

| Storyline | ATIF |
|---|---|
| `session` | `session_id` |
| `turns[i].id` / `src` / `msg` | `steps[i].step_id` / `source` / `message` |
| `turns[i].tool_calls` | `steps[i].tool_calls` |
| `turns[i].latency_ms` | `steps[i].metrics.latency_ms` |

---

## 文件与探测

| 约定 | 值 |
|---|---|
| 文件名 | `storyline.json` / `{session}.storyline.json` |
| 内容 | `session` + `turns` |

实现：`into_storyline` / `from_storyline` / `convert`。

---

## History

| Date | Notes |
|---|---|
| 2026-07-30 | 初稿与迭代（hub、短名、去 `calls[]`、性能字段、`session`） |
| 2026-07-30 | 收敛为 ATIF-first：去掉 Capture Call/Normal 过度叙事；`continued_trajectory_ref` 对齐 ATIF；`parent.scid` 可选 |
