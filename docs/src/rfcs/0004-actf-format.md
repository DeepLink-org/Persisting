# RFC-0004: ACTF v1.0 轨迹格式

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Format** | `actf` |
| **Date** | 2026-08-07 |
| **Component** | `persisting-pchronicle` |
| **Source fixture** | `data/make-doom-for-mips_software-engineering.json` |
| **Implements** | `crates/persisting-pchronicle/src/formats/actf.rs` · `crates/persisting-pchronicle/src/convert/actf.rs` |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [RFC-0003 pChronicle Ownership](0003-pchronicle-ownership.md) · [RFC-0008 ATIF](0008-atif-format.md) · [RFC-0009 OpenAI Messages](0009-openai-messages-format.md) |

## 摘要

ACTF v1.0 是以 benchmark task 为根、以编号 attempt 为分支、以结构化 agent step 为
轨迹单元的 JSON 格式。pChronicle 将 `actf` 作为 Storyline hub 的外围格式；每个
attempt 转换为一条 Storyline，并写入既有的 `runs.lance`、`steps.lance`、
`tool_calls.lance` 三表。ACTF 不引入第四张表，也不改变三表 Arrow schema。

## JSON 数据模型

以下字段由样本抽象为 ACTF v1.0 的稳定核心。所有对象允许未知字段，解析器必须将其作为
opaque JSON 扩展保留。

```text
ActfDocument
├── task_id: string
├── category: string
├── k: uint
├── correct: bool
├── attempts_tried: uint
├── solved_at: string | null
└── attempts: map<string, ActfAttempt>

ActfAttempt
├── correct: bool
├── final_answer: any | null
├── ground_truth: string
├── status: string
├── score: any | null
├── error: string
├── artifacts / extra / analysis_result / meta: any
└── trajectory: ActfTrajectory

ActfTrajectory
├── schema_version: "ACTF_v1.0"
├── started_at / finished_at: string
└── steps: ActfStep[]

ActfStep
├── step_id: integer
├── assistant_content: { content, reasoning_content, tool_calls }
├── metric: { prompt_tokens_len, completion_tokens_len,
│             llm_infer_ms, env_action_ms, stop_reason }
├── system_prompt / user_content: string
├── tools: ActfToolCall[]
├── observation: ActfObservation[]
└── started_at / finished_at: string

ActfToolCall       = { type: string, id: string, ...event-specific fields }
ActfObservation    = { type: string, id?: string, tool_use_id?: string,
                       ...event-specific fields }
```

token 数、`llm_infer_ms` 和 `env_action_ms` 可以是数值或 `null`，`stop_reason` 也可以
显式为 `null`；解析器必须接受两种表示，进入 Storyline 后 missing/null 按同一语义默认值
规范化。ACTF v1.0 已观察到两种工具事件：
`tool_use` 使用 `name/input` 与 `tool_use_id/content`，`command_execution` 使用
`command/aggregated_output/exit_code/status`，并通过共同的 `id` 关联。事件专属字段作为
opaque JSON 保留。

## 校验约束

- `task_id`、`category`、attempt id、attempt `status` 必须非空，`k > 0`。
- `attempts` 必须非空；`attempts_tried` 等于实际 attempt 数且不得大于 `k`。
- trajectory 的 `schema_version` 必须是 `ACTF_v1.0`，时间字段必须非空。
- step id 必须为正数并严格递增。
- 同一步内 tool call id 唯一，`assistant_content.tool_calls` 与 `tools` 相等。
- observation 存在 `tool_use_id` 或 `id` 时，必须引用同一步的 tool call。

根级 `correct`、`solved_at` 与 attempt 结果之间不施加样本之外的推导约束；它们按输入
原值保存。

## ACTF → Storyline JSON Pointer 映射 {#actf-storyline-json-pointer-mapping}

本节是 ACTF 到 Storyline 字段映射的权威定义。指针遵循 RFC 6901；`{a}`、`{s}`、
`{c}`、`{o}` 和 `{t}` 分别表示 attempt key、源 step 下标、tool call 下标、observation
result 下标和目标 turn 下标。代入实际 token 后即为普通 JSON Pointer。

`P` 表示左侧命中的完整源 pointer；`E(P)` 表示把整个 `P` 作为 `fields` 对象 key 后再做
一次 RFC 6901 token 转义。所有输出都生成
`/schema_version = "storyline/v1"`、`/origin/format = "actf"`，并从
`/unknown_fields` 计算 `/unknown_key_counts`；这些值没有源 pointer，故不列入表。

一个 ACTF document 可以包含多个 attempts，因此映射基数为：

```text
ACTF document 1 ──► N Storyline runs ──► 原有 Lance 三表
ACTF attempt  1 ──► 1 Storyline run
ACTF step     1 ──► 1 Storyline step
ACTF tool     1 ──► 1 Storyline tool_call
```

| ACTF JSON Pointer | Storyline JSON Pointer |
| --- | --- |
| `/task_id` | `/run`<br>`/session`<br>`/unknown_fields/sources/actf/source_document_id` |
| `/correct` | `/final_metrics/task_correct` |
| `/attempts/{a}` | `/attempt_id`<br>`/session` |
| `/attempts/{a}/correct` | `/final_metrics/correct` |
| `/attempts/{a}/score` | `/final_metrics/score` |
| `/attempts/{a}/status` | `/final_metrics/status` |
| `/attempts/{a}/analysis_result` | `/final_metrics/analysis_result` |
| `/attempts/{a}/trajectory/schema_version` | `/origin/schema_version` |
| `/attempts/{a}/trajectory/steps/{s}/step_id` | `/turns/{t}/id` |
| `/attempts/{a}/trajectory/steps/{s}/started_at` | `/turns/{t}/ts` |
| `/attempts/{a}/trajectory/steps/{s}/assistant_content/content` | `/turns/{t}/msg` |
| `/attempts/{a}/trajectory/steps/{s}/assistant_content/reasoning_content` | `/turns/{t}/reason` |
| `/attempts/{a}/trajectory/steps/{s}/tools`<br>`/attempts/{a}/trajectory/steps/{s}/assistant_content/tool_calls` | `/turns/{t}/tool_calls`<br>`/turns/{t}/kind` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/id`<br>`/attempts/{a}/trajectory/steps/{s}/assistant_content/tool_calls/{c}/id` | `/turns/{t}/tool_calls/{c}/tcid` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/name`<br>`/attempts/{a}/trajectory/steps/{s}/assistant_content/tool_calls/{c}/name` | `/turns/{t}/tool_calls/{c}/fn`<br>`/turns/{t}/tool_calls/{c}/args/name` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/type`<br>`/attempts/{a}/trajectory/steps/{s}/assistant_content/tool_calls/{c}/type` | `/turns/{t}/tool_calls/{c}/fn`<br>`/unknown_fields/sources/actf/fields/{E(P)}` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/input`<br>`/attempts/{a}/trajectory/steps/{s}/assistant_content/tool_calls/{c}/input` | `/turns/{t}/tool_calls/{c}/args` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/command`<br>`/attempts/{a}/trajectory/steps/{s}/assistant_content/tool_calls/{c}/command` | `/turns/{t}/tool_calls/{c}/args/command` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/aggregated_output`<br>`/attempts/{a}/trajectory/steps/{s}/assistant_content/tool_calls/{c}/aggregated_output` | `/turns/{t}/tool_calls/{c}/result`<br>`/turns/{t}/tool_calls/{c}/args/aggregated_output` |
| `/attempts/{a}/trajectory/steps/{s}/metric/prompt_tokens_len` | `/turns/{t}/metrics/prompt_tokens_len` |
| `/attempts/{a}/trajectory/steps/{s}/metric/completion_tokens_len` | `/turns/{t}/metrics/completion_tokens_len` |
| `/attempts/{a}/trajectory/steps/{s}/metric/llm_infer_ms` | `/turns/{t}/metrics/llm_infer_ms`<br>`/turns/{t}/latency_ms` |
| `/attempts/{a}/trajectory/steps/{s}/metric/env_action_ms` | `/turns/{t}/metrics/env_action_ms`<br>`/turns/{t}/tool_calls/0/duration_ms` |
| `/attempts/{a}/trajectory/steps/{s}/metric/stop_reason` | `/turns/{t}/metrics/stop_reason` |
| `/attempts/{a}/trajectory/steps/{s}/metric/{other-metric}` | `/turns/{t}/metrics/{other-metric}` |
| `/attempts/{a}/trajectory/steps/{s}/observation/{o}` | `/turns/{t}/observation/results/{o}` |
| `/attempts/{a}/trajectory/steps/{s}/observation/{o}/tool_use_id` | `/turns/{t}/observation/results/{o}/tool_use_id`<br>`/turns/{t}/observation/results/{o}/source_call_id` |
| `/attempts/{a}/trajectory/steps/{s}/observation/{o}/id` | `/turns/{t}/observation/results/{o}/id`<br>`/turns/{t}/observation/results/{o}/source_call_id` |
| `/attempts/{a}/trajectory/steps/{s}/observation/{o}/content` | `/turns/{t}/observation/results/{o}/content` |
| `/attempts/{a}/trajectory/steps/{s}/observation/{o}/aggregated_output` | `/turns/{t}/observation/results/{o}/aggregated_output`<br>`/turns/{t}/observation/results/{o}/content` |
| `/attempts/{a}/trajectory/steps/{s}/observation/{o}/{other-field}` | `/turns/{t}/observation/results/{o}/{other-field}` |
| `/category`<br>`/k`<br>`/attempts_tried`<br>`/solved_at`<br>`/{other-root-key}` | `/unknown_fields/sources/actf/fields/{E(P)}` |
| `/attempts/{a}/final_answer`<br>`/attempts/{a}/ground_truth`<br>`/attempts/{a}/error`<br>`/attempts/{a}/artifacts`<br>`/attempts/{a}/extra`<br>`/attempts/{a}/meta`<br>`/attempts/{a}/{other-attempt-key}` | `/unknown_fields/sources/actf/fields/{E(P)}` |
| `/attempts/{a}/trajectory/started_at`<br>`/attempts/{a}/trajectory/finished_at`<br>`/attempts/{a}/trajectory/{other-trajectory-key}` | `/unknown_fields/sources/actf/fields/{E(P)}` |
| `/attempts/{a}/trajectory/steps/{s}/system_prompt`<br>`/attempts/{a}/trajectory/steps/{s}/user_content`<br>`/attempts/{a}/trajectory/steps/{s}/finished_at`<br>`/attempts/{a}/trajectory/steps/{s}/{other-step-key}` | `/unknown_fields/sources/actf/fields/{E(P)}` |
| `/attempts/{a}/trajectory/steps/{s}/assistant_content/{other-assistant-key}` | `/unknown_fields/sources/actf/fields/{E(P)}` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/{other-tool-key}`<br>`/attempts/{a}/trajectory/steps/{s}/assistant_content/tool_calls/{c}/{other-tool-key}` | `/turns/{t}/tool_calls/{c}/args/{other-tool-key}`<br>`/unknown_fields/sources/actf/fields/{E(P)}` |

条件和规范化规则：

- 一个 `/attempts/{a}` 生成一份 Storyline。单 attempt 时 `/session = task_id`；多
  attempt 时 `/session = "{task_id}#attempt-{a}"`。
- `/agent/id` 固定为 `actf-agent`，`/agent/name` 固定为 `ACTF Agent`。每个 turn 固定
  `src=agent, nllm=1`；存在非空 tools 时 `kind=autonomous`。
- `tools` 是规范化 tool-call 来源；`assistant_content.tool_calls` 必须与其完全相等。
  `name` 优先作为 `/fn`，缺失或为空时回退到 `type`。
- arguments 按 `input → {"command": command} → flattened tool fields object` 选择。
  因此 `/args/name`、`/args/aggregated_output` 和 `/args/{other-tool-key}` 只在 `input`
  与 `command` 都不存在时产生；无论 `{other-tool-key}` 是否进入 `/args`，它仍以源 pointer
  保存在 `unknown_fields`。`type` 也总是额外保留，以便同源恢复。
- `env_action_ms` 仅在该 step 恰有一个 tool call 且值为 number 时，才同时写入该 call
  的 `/duration_ms`。所有 metric 字段始终保留在 `/metrics`。
- observation result 整体进入 `/observation/results`。`tool_use_id` 优先于 `id` 生成
  `source_call_id`；`aggregated_output` 优先于 `content` 生成规范化 `content`，原字段仍
  保留在 result 内。
- unknown root 字段附到该 document 产生的每个 Storyline attempt；同源恢复时要求它们
  一致。恢复使用 `/run`、`/attempt_id` 和 ACTF unknown fields 重组 attempt map；跨格式
  转换通过 version-1 `_storyline` envelope 携带外来 unknown fields。

## 保真边界

ACTF → Storyline → 三表 Lance → Storyline → ACTF 保证规范化 JSON 数据模型级语义一致：
未知键及其值（包括 `null`）、嵌套值、数组顺序和 attempt 分组均保留。已知字段的
missing/显式 `null` 会按 Storyline 语义规范化；源文件空白、缩进和对象键顺序不属于保真
边界。没有 ACTF source unknown fields 的普通 Storyline 可以导出为结构合法的单 attempt ACTF，
但这是有定义的合成转换，不宣称还原某个原始 ACTF 文件。
