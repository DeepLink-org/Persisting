# Storyline `task` / `env` / tool `response` 扩展

## Status

Draft. Approved in conversation on 2026-08-22; amended the same day to put
eval budget on `/task`, `k` on `/task/llm`, and `started_at` / `finished_at`
on the Storyline document (plus turn-level `finished_at`).

## Context

ACTF 与 OpenAI Messages 导入 Storyline 时，评测结果、运行时环境和工具执行状态
大量落入 `unknown_fields`。Storyline 现有 schema 是 ATIF-first：有 `final_metrics`
和任意 JSON 的 `tool_calls[].result`，没有文档级任务对象，也没有步级环境变化。

本设计给 `storyline/v1` 增加可选结构，不升 schema 版本：

1. 根对象 `/task`：文档级环境、LLM 推理参数（先收 `k`）、评测结果与评测预算
2. 根对象 `/started_at`、`/finished_at`：文档级时间窗
3. `/turns/{t}/env`：相对 `/task/env` 的环境变化
4. `/turns/{t}/finished_at`：该 turn 的结束时间（开始时间仍是 `/ts`）
5. `/tool_calls/{c}/kind` 与 `/tool_calls/{c}/response`：工具事件类型与结构化执行结果

`agent.model`、`turns[].model`、`effort` 仍是 model / thinking 的权威字段；本设计
不把它们搬进 `/task/llm`。ACTF 的 `system_prompt` / `user_content` 不是运行时环境，
本设计不吸收；它们改由
[2026-08-22-storyline-prompt-design.md](2026-08-22-storyline-prompt-design.md)
的 `/prompt` 承接。

## Goals

1. 把两次导入 warning 中的评测结果、评测预算、文档/步时间窗、OpenAI `env_state`
   基础设施键、以及 ACTF 工具 `type` / `status` / `exit_code` 提升为 Storyline
   领域字段。`k` 作为文档级 LLM 推理参数进入 `/task/llm/k`。
2. 每个源 JSON Pointer 仍只有一个权威目标；`/final_metrics` 上的
   `task_correct` / `correct` / `status` / `score` 改为从 `/task/result` 提升。
3. 步级环境按「相对文档环境的差异」存储，还原时用 `/task/env` 覆盖该 turn 的
   `env`，不要求折叠更早的 turns。
4. Lance 仍是 `runs` / `steps` / `tool_calls` 三表，不新增第四张表。

## Non-goals

- 把 ACTF `system_prompt` / `user_content` 提升为一等字段（见后续 `/prompt` 规格）。
- 把 attempt `extra` / `meta` 提升为一等字段。
- 把已进入 `/metrics` 的 `env_state` 键（token、latency、`status_code`、
  `finish_reason` 等）再复制进 `env`。
- 把 `agent.model` / `turns[].model` / `effort` 的权威目标改到 `/task/llm`。
- 新增 `storyline/v2`，或修改 ATIF / AgenticMD / Events 的一等 schema。
- 把 `env_state` 展平为独立 SQL 列。
- 改变 TTAS、Queue、Search、`persisting-dlcapt`。

## Wire schema（RFC-0001 增补）

`schema_version` 仍为 `storyline/v1`。新对象全部 optional；缺省或空对象不序列化。
拥有字段的对象继续 `deny_unknown_fields`；`env.state` 的值是开放 JSON object。

保持全名（不引入短名）：`task` / `env` / `llm` / `result` / `response` /
`started_at` / `finished_at`。`tool_calls[].kind` 与 `turns[].kind` 同名不同槽：
前者是工具事件类型，后者是叙事种类。`turns[].effective_kind()` 不得读取
`tool_calls[].kind`。

根对象增加：

| Wire | Type | Status |
|---|---|---|
| `task` | object | Optional |
| `started_at` | string \| number | Optional；与 turn `ts` 相同的时间表示 |
| `finished_at` | string \| number | Optional；与 turn `ts` 相同的时间表示 |

`task` 保持全名（与 `agent` 相同）。内部：

```text
/task
├── env                         文档级环境：身份与稳定配置
│   ├── name                    string?
│   ├── endpoint                string?
│   ├── id                      string?
│   ├── event_type              string?
│   ├── request_id              string?
│   └── state                   object?   其余稳定基础设施键
├── llm                         文档级 LLM 推理参数
│   └── k                       integer?  ACTF `/k`；不是采样温度
└── result                      评测结果 + 文档级评测预算
    ├── task_correct            bool?     源文档根级 correct（可与 attempt 不同）
    ├── correct                 bool?
    ├── final_answer            any?
    ├── ground_truth            any?
    ├── status                  string?
    ├── score                   any?
    ├── error                   string?
    ├── artifacts               any?
    ├── category                string?
    ├── attempts_tried          integer?
    ├── solved_at               string?
    ├── retry_count             any?
    └── retry_counts            any?
```

`/k` 的权威目标是 `/task/llm/k`，不是 `/task/result`。ACTF 源里它是 attempt 预算，
Storyline 按调用方约定把它放在 LLM 推理参数槽；导出 ACTF 仍写回根 `/k`。

`turns[]` 增加：

| Wire | Type | Status |
|---|---|---|
| `env` | object | Optional；形状与 `/task/env` 相同 |
| `finished_at` | string \| number | Optional；该 turn 结束时间，表示规则与 `ts` 相同 |

开始时间仍是 `/turns/{t}/ts`，不新增 turn 级 `started_at`。`copied == true` 的
context turn 不得写 `env`。OpenAI 一行拆成 request + response 时，`env` 只挂在
response turn。`finished_at` 同样只写在产生该 step 语义的 turn 上（ACTF：该
step 对应的唯一 turn；OpenAI：response turn）。

`tool_calls[]` 增加：

| Wire | Type | Status |
|---|---|---|
| `kind` | string | Optional；工具事件类型，与 turn 的叙事 `kind` 不是同一个字段 |
| `response` | object | Optional |

```text
/tool_calls/{c}/response
├── status                      string?
└── exit_code                   integer?
```

现有 `result` 仍是输出体（ACTF `aggregated_output`）。没有 `status` 且没有
`exit_code` 时不写 `response`。

校验增补：

- `/task` 若存在，则 `env`、`llm`、`result` 至少有一个含非 null 字段。
- `/started_at`、`/finished_at`、`turns[].finished_at` 的合法值与 `ts` 相同：
  RFC3339 字符串，或可精确表示为纳秒的 Unix epoch 秒数。
- 若文档同时有 `/started_at` 与 `/finished_at`，不得要求 `finished_at >= started_at`
  以外的推导（源格式可以记录与 turn 窗口不一致的轨迹时间）。
- `task.llm.k` 若出现，必须是正整数。
- `tool_calls[].kind` 非空字符串；空字符串视为缺失。
- `exit_code` 必须是整数（含负值）；JSON number 带小数则导入失败。
- 还原某 turn 的完整环境：以 `/task/env` 为底，对象浅合并该 turn 的 `env`
  （turn 侧同名键覆盖；`state` 也是浅合并）。不合并更早的 turns。
  查询层按此规则计算；存储层不物化合并结果。

## 环境键分流

文档级 `/task/env` 只保存 session 内作为默认配置的键。步级 `/turns/{t}/env`
只保存本步才有、或相对文档环境发生变化的键。

首个为该键提供非空值的 OpenAI row 写入 `/task/env`（见下表「稳定键」）。后续
row：值相等则视为冗余别名并消费；值不等则写入该 row 的 response turn
`/env`（环境变化）。不得因后续行不一致而失败。

只存在于步上的键从不写入 `/task/env`。

已进入 `/metrics` 的 `env_state` 键不进入 `env`。OpenAI `env_state.created_at` /
`completed_at` 仍只在 `/metrics`，不提升为文档 `/started_at` / `/finished_at`。

## ACTF 映射变更（RFC-0004）

权威目标搬家或从残差提升：

| ACTF JSON Pointer | 新权威目标 |
|---|---|
| `/correct` | `/task/result/task_correct` |
| `/k` | `/task/llm/k` |
| `/category` | `/task/result/category` |
| `/attempts_tried` | `/task/result/attempts_tried` |
| `/solved_at` | `/task/result/solved_at` |
| `/retry_count` | `/task/result/retry_count` |
| `/retry_counts` | `/task/result/retry_counts` |
| `/attempts/{a}/correct` | `/task/result/correct` |
| `/attempts/{a}/status` | `/task/result/status` |
| `/attempts/{a}/score` | `/task/result/score` |
| `/attempts/{a}/final_answer` | `/task/result/final_answer` |
| `/attempts/{a}/ground_truth` | `/task/result/ground_truth` |
| `/attempts/{a}/error` | `/task/result/error` |
| `/attempts/{a}/artifacts` | `/task/result/artifacts` |
| `/attempts/{a}/trajectory/started_at` | `/started_at` |
| `/attempts/{a}/trajectory/finished_at` | `/finished_at` |
| `/attempts/{a}/trajectory/steps/{s}/finished_at` | `/turns/{t}/finished_at` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/type` | `/turns/{t}/tool_calls/{c}/kind` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/status` | `/turns/{t}/tool_calls/{c}/response/status` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/exit_code` | `/turns/{t}/tool_calls/{c}/response/exit_code` |

`steps/{s}/started_at` 仍只映射到 `/turns/{t}/ts`，即使它与文档 `/started_at`
数值相同，也不把文档时间当成 step 时间的第二权威目标。

`assistant_content.tool_calls` 必须与 `tools` 相等；其上的 `type` / `status` /
`exit_code` 视为已消费，不另写目标，不进残差。

`name` 为空或缺失时 `/fn` 仍可由同 call 的 `kind` 派生。`type` 的唯一权威目标是
`/kind`，`/fn` 不是它的第二权威目标。

便利提升：将 `/task/result` 的 `task_correct` / `correct` / `status` / `score`
再写入 `/final_metrics` 同名键。`analysis_result` 仍只在 `/final_metrics`。

ACTF 没有 OpenAI `env_state` 时，`/task/env` 与 `/turns/{t}/env` 缺省。

attempt `extra` / `meta` 现进入文档 `/extra` / `/meta`，不再作为残差。

`system_prompt` / `user_content` 不再由本规格吸收；见
[2026-08-22-storyline-prompt-design.md](2026-08-22-storyline-prompt-design.md)。

空字符串 `error`、空 `artifacts`、空 `solved_at` 按缺失省略。

## OpenAI Messages 映射变更（RFC-0009）

稳定键（首条非空值 → `/task/env`；后续相等则消费）：

| OpenAI JSON Pointer | Storyline JSON Pointer |
|---|---|
| `/session_steps/{r}/env_name` | `/task/env/name` |
| `/session_steps/{r}/meta_json/env_state/endpoint` | `/task/env/endpoint` |
| `/session_steps/{r}/dataset_type` | `/task/env/state/dataset_type` |
| `/session_steps/{r}/dt` | `/task/env/state/dt` |
| `/session_steps/{r}/meta_json/group_id` | `/task/env/state/group_id` |
| `/session_steps/{r}/meta_json/env_state/redaction_policy` | `/task/env/state/redaction_policy` |
| `/session_steps/{r}/meta_json/env_state/upstream_base_url` | `/task/env/state/upstream_base_url` |
| `/session_steps/{r}/meta_json/env_state/weight_version` | `/task/env/state/weight_version` |

步级键（只进 response turn）：

| OpenAI JSON Pointer | Storyline JSON Pointer |
|---|---|
| `/session_steps/{r}/id` | `/turns/{response-t}/env/id` |
| `/session_steps/{r}/meta_json/env_state/event_type` | `/turns/{response-t}/env/event_type` |
| `/session_steps/{r}/meta_json/env_state/request_id` | `/turns/{response-t}/env/request_id` |

OpenAI 没有 ACTF 评测结果、`k`、文档时间窗或工具 `response` 时，对应 Storyline
字段缺省。row `created_at` 仍只权威写入 response `/ts`，不写文档 `/started_at`。

## Lance 投影

不新增表。JSON 列保存完整对象，不把 `env_state` 展平为独立列。

| 表 | 新列 | 内容 |
|---|---|---|
| `runs` | `task` | `/task` 对象，或缺省 |
| `runs` | `started_at` | 文档开始时间，或缺省 |
| `runs` | `finished_at` | 文档结束时间，或缺省 |
| `steps` | `env` | 该 turn 的 `/env`，或缺省 |
| `steps` | `finished_at` | 该 turn 结束时间，或缺省 |
| `tool_calls` | `kind` | 字符串，或缺省 |
| `tool_calls` | `response` | `{status, exit_code}` 对象，或缺省 |

查询完整环境时由查询层合并 `runs.task.env` 与 `steps.env`；存储层不物化合并结果。

## 跨格式

- ACTF ↔ Storyline、OpenAI Messages ↔ Storyline：按上表还原。
- Storyline 一等字段不是 `unknown_fields`。ATIF 本设计不新增一等键；Storyline →
  ATIF 时，ATIF 无法表达的 `/task`、`/started_at`、`/finished_at`、`turns[].env`、
  `turns[].finished_at`、`tool_calls[].kind` / `response` 走既有 `_storyline`
  envelope，不得塞进 ATIF `extra`（`extra` 仍 1:1 对应 Storyline `extra`）。
- AgenticMD / Events 同理：不扩张其 schema；经 Storyline 的 roundtrip 必须能恢复
  这些新字段。

## 实现落点（文档，非本阶段改代码）

- `crates/persisting-pchronicle/src/formats/storyline.rs`：wire 结构
- `crates/persisting-pchronicle/src/store/storyline/model.rs`：三表行
- `crates/persisting-pchronicle/src/convert/actf.rs` 与 RFC-0004
- `crates/persisting-pchronicle/src/formats/openai_corpus.rs` 与 RFC-0009
- `docs/src/rfcs/0001-storyline-format.md` 增补 wire 表

## 验收

1. 用产生原 ACTF warning 的语料导入 `storyline-lance` 后，下列 key 不再出现在
   unknown-field warning：`artifacts`、`error`、`final_answer`、`ground_truth`、
   `category`、`k`、`attempts_tried`、`solved_at`、`retry_count`、`retry_counts`、
   `trajectory/started_at`、`trajectory/finished_at`、`steps/*/finished_at`、
   `tools/*/type`、`tools/*/status`、`tools/*/exit_code`，以及
   `assistant_content/tool_calls` 上的同名三键。
2. 用产生原 OpenAI warning 的 `session_steps.json` 导入后，下列 key 不再 warning：
   `dataset_type`、`dt`、`env_name`、`id`、`meta_json/group_id`、
   `env_state/endpoint`、`event_type`、`redaction_policy`、`request_id`、
   `upstream_base_url`、`weight_version`。
3. ACTF → Storyline → ACTF 还原上表权威字段；`type` 从 `/kind` 还原，`k` 从
   `/task/llm/k` 还原，不再依赖 `unknown_fields`。
4. OpenAI → Storyline → OpenAI 还原上表环境键；session 内变化的稳定键出现在对应
   step 的源字段上，而不是被首条覆盖。
5. 既有 ATIF fixture 与 `final_metrics` 提升行为保持：有 `/task/result` 时
   `/final_metrics` 含对应键；无 `/task` 的旧文档仍合法。
6. 仍为残差（允许继续 warning）：attempt `extra`、`meta`。
   `system_prompt` / `user_content` 改由 `/prompt` 规格验收，不再列为本规格残差。
