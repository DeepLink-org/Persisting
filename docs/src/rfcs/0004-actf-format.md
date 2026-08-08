# RFC-0004: ACTF v1.0 轨迹格式

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Date** | 2026-08-07 |
| **Component** | `persisting-pchronicle` |
| **Source fixture** | `data/make-doom-for-mips_software-engineering.json` |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [RFC-0003 pChronicle Ownership](0003-pchronicle-ownership.md) |

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
显式为 `null`；实现不得把显式 `null` 静默改成字段缺失。ACTF v1.0 已观察到两种工具事件：
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

## Storyline 与 Lance 映射

一个 ACTF document 可以包含多个 attempts，因此映射基数为：

```text
ACTF document 1 ──► N Storyline runs ──► 原有 Lance 三表
ACTF attempt  1 ──► 1 Storyline run
ACTF step     1 ──► 1 Storyline step
ACTF tool     1 ──► 1 Storyline tool_call
```

- `task_id` 写入 `run_id`；单 attempt 的 `session_id` 使用 `task_id`，多 attempt 使用
  `{task_id}#attempt-{attempt_id}`。
- assistant `content`、`reasoning_content`、tool call 和 observation 投影到对应的
  Storyline 字段，token/latency 投影到 metrics。
- ACTF 根、attempt、trajectory 元数据和完整原始 step 写入三表现有的 `extra_json`
  扩展列，并带 `_pchronicle_actf` provenance version。
- 恢复时以 provenance 重组 attempt map；多个 attempt 必须具有相同根元数据。

## 保真边界

ACTF → Storyline → 三表 Lance → Storyline → ACTF 保证 JSON 数据模型级无损：键值、显式
`null`、未知字段、嵌套值、数组顺序和 attempt 分组均保留。它不保证源文件空白、缩进或
对象键顺序逐字节一致。没有 ACTF provenance 的普通 Storyline 可以导出为结构合法的
单 attempt ACTF，但这是有定义的合成转换，不宣称还原某个原始 ACTF 文件。
