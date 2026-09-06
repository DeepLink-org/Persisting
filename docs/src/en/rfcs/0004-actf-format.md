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
`tool_calls.lance` 三表。新字段作为这三张表上的可空 JSON/时间列投影，不引入第四张表。

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
├── ground_truth: string | object | any
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

ActfToolCall       = { type?: string, id?: string, name?: string,
                       arguments? | input? | command?, ...event-specific fields }
ActfObservation    = { type?: string, id?: string, tool_use_id?: string,
                       ...event-specific fields }
```

token 数、`llm_infer_ms` 和 `env_action_ms` 可以是数值或 `null`，`stop_reason` 也可以
显式为 `null`。`assistant_content.content` / `reasoning_content`、`system_prompt` /
`user_content`、attempt `error` / `status` 也可以是字符串或 `null`；`null` 与缺省、空字符串
按同一语义规范化（空 `reasoning_content` 使 `/reason` 缺省；空 `error` 使
`/task/result/error` 缺省；空 `status` 仍校验失败）。解析器必须接受两种表示，进入
Storyline 后 missing/null 按同一语义默认值规范化。ACTF v1.0 已观察到两种工具事件：
`tool_use` 使用 `name/input` 与 `tool_use_id/content`，`command_execution` 使用
`command/aggregated_output/exit_code/status`，并通过共同的 `id` 关联。事件专属字段作为
opaque JSON 保留。

## 校验约束

- `task_id`、`category`、attempt id 必须非空，`k > 0`。attempt `status` 可为空。
- `attempts` 必须非空；`attempts_tried` 等于实际 attempt 数且不得大于 `k`。
- trajectory 的 `schema_version` 必须是 `ACTF_v1.0`，时间字段必须非空。失败 attempt
  允许把 `trajectory` 写成 OpenClaw 事件数组（`session` / `message` / `model_change`
  等），而不是 `{schema_version, steps, ...}` 对象。这是 **ACTF 专属有损入口**，不是
  独立 `DocumentFormat`，探测仍认作 ACTF。导入时 `message.role=user|assistant|toolResult`
  收成 Storyline turns，`toolResult` 折回前一条 assistant 的 `tool_calls`。导出写回
  canonical ACTF trajectory **对象**（含 `schema_version` / `steps`），不还原为事件数组。
- step id 必须为正数并严格递增。
- 同一步内 tool call id 唯一（缺 `id` 时按 `step-{step_id}-tool-{index}` 合成后再比）。`tools` 与 `assistant_content.tool_calls` 都非空时必须相等；一侧为空时以非空一侧为工具来源。`type` / `id` 可选；`{name, arguments}` 与 OpenAI `{id,type,function:{name,arguments}}` 都合法。attempt `status` 可为空。
- observation 的 `type` 可选；仅有 `content` 的环境输出合法。存在 `tool_use_id` 或 `id` 时，必须引用同一步的 tool call。

根级 `correct`、`solved_at` 与 attempt 结果之间不施加样本之外的推导约束；它们按输入
原值保存。

## ACTF → Storyline JSON Pointer 映射 {#actf-storyline-json-pointer-mapping}

本节是 ACTF 到 Storyline 字段映射的权威定义。指针遵循 RFC 6901。Storyline 强类型 hub 只保留通用评测字段（`correct` / `score` / `status` / `error` / `final_answer` / `ground_truth` / `artifacts` / `max_score`）；表中的 ACTF 私货目标（`/task/result/task_correct`、`category`、`attempts_tried`、`solved_at`、`retry_count`、`retry_counts`）落在 `task.result` extra，JSON Pointer 不变。`{a}`、`{s}`、
`{c}`、`{o}` 和 `{t}` 分别表示 attempt key、源 step 下标、tool call 下标、observation
result 下标和目标 turn 下标。代入实际 token 后即为普通 JSON Pointer。

映射分成四类，禁止把不同角色的槽写进同一格：

| 类别 | 含义 | 每个源 pointer 的目标数 |
| --- | --- | --- |
| 权威字段 | ACTF 值进入一个 Storyline 领域字段；同源导出从该字段还原 | 恰好 1 |
| 派生身份 | 由一个或多个源值计算，不是字段拷贝 | 公式，见下 |
| 便利提升 | 在权威字段之外再复制到 hub 顶栏或别名 | 额外 0 或 1，不替代权威字段 |
| 残差 | Storyline 没有对应领域字段，按原 pointer 进入 `unknown_fields` | 1（残差槽） |

`P` 表示左侧命中的完整源 pointer；`E(P)` 表示把整个 `P` 作为 `fields` 对象 key 后再做
一次 RFC 6901 token 转义。

基数：

```text
ACTF document  1 ──► N Storyline 文档（共用 /run = task_id）
ACTF attempt   1 ──► 1 Storyline 文档
ACTF step      1 ──► 1 Storyline turn
ACTF tool      1 ──► 1 Storyline tool_call
```

`/turns/{t}` 与源 `steps[{s}]` 保持同一数组顺序。`tools` 是 tool-call 的权威来源；
`tools` 非空时是 tool-call 的权威来源；`tools` 为空则回退到
`assistant_content.tool_calls`。两侧都非空时必须完全相等，不另写一套目标。
OpenAI `function.name` / `function.arguments` 分别映射到 `/fn` 与 `/args`，`function`
视为已消费。

### 权威字段（1:1）

| ACTF JSON Pointer | Storyline JSON Pointer |
| --- | --- |
| `/task_id` | `/run` |
| `/correct` | `/task/result/task_correct` |
| `/category` | `/task/result/category` |
| `/k` | `/task/llm/k` |
| `/attempts_tried` | `/task/result/attempts_tried` |
| `/solved_at` | `/task/result/solved_at` |
| `/retry_count` | `/task/result/retry_count` |
| `/retry_counts` | `/task/result/retry_counts` |
| `/attempts/{a}` 的 map key `{a}` | `/attempt_id` |
| `/attempts/{a}/correct` | `/task/result/correct` |
| `/attempts/{a}/score` | `/task/result/score` |
| `/attempts/{a}/max_score` | `/task/result/max_score` |
| `/attempts/{a}/status` | `/task/result/status` |
| `/attempts/{a}/extra` | `/extra` |
| `/attempts/{a}/meta` | `/meta` |
| `/attempts/{a}/final_answer` | `/task/result/final_answer` |
| `/attempts/{a}/ground_truth` | `/task/result/ground_truth` |
| `/attempts/{a}/error` | `/task/result/error` |
| `/attempts/{a}/artifacts` | `/task/result/artifacts` |
| `/attempts/{a}/analysis_result` | `/final_metrics/analysis_result` |
| `/attempts/{a}/trajectory/schema_version` | `/origin/schema_version` |
| `/attempts/{a}/trajectory/started_at` | `/started_at` |
| `/attempts/{a}/trajectory/finished_at` | `/finished_at` |
| `/attempts/{a}/trajectory/steps/{s}/step_id` | `/turns/{t}/id` |
| `/attempts/{a}/trajectory/steps/{s}/started_at` | `/turns/{t}/ts` |
| `/attempts/{a}/trajectory/steps/{s}/finished_at` | `/turns/{t}/finished_at` |
| `/attempts/{a}/trajectory/steps/{s}/assistant_content/content` | `/turns/{t}/msg` |
| `/attempts/{a}/trajectory/steps/{s}/system_prompt` | 文档 `/prompt/system` 或 `/turns/{t}/prompt/system`（见下） |
| `/attempts/{a}/trajectory/steps/{s}/user_content` | 文档 `/prompt/user` 或 `/turns/{t}/prompt/user`（见下） |
| `/attempts/{a}/trajectory/steps/{s}/assistant_content/reasoning_content` | `/turns/{t}/reason` |
| `/attempts/{a}/trajectory/steps/{s}/tools` | `/turns/{t}/tool_calls` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/id` | `/turns/{t}/tool_calls/{c}/tcid` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/name` | `/turns/{t}/tool_calls/{c}/fn` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/type` | `/turns/{t}/tool_calls/{c}/kind` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/input` | `/turns/{t}/tool_calls/{c}/args` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/command` | `/turns/{t}/tool_calls/{c}/args/command` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/aggregated_output` | `/turns/{t}/tool_calls/{c}/result` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/status` | `/turns/{t}/tool_calls/{c}/response/status` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/exit_code` | `/turns/{t}/tool_calls/{c}/response/exit_code` |
| `/attempts/{a}/trajectory/steps/{s}/metric` | `/turns/{t}/metrics` |
| `/attempts/{a}/trajectory/steps/{s}/observation` | `/turns/{t}/observation/results` |

`system_prompt` / `user_content` 按 pair 去重。第一个至少一侧非空的 pair 写入文档
`/prompt`（空字符串键省略）。后续 step：pair 与基线相同则已消费，turn 不写
`/prompt`；不同则该 turn `/prompt` 写当前完整 pair（整段覆盖，不是浅合并）。基线之前的
双空 step 写 `{"system":"","user":""}`。导出时
`system_prompt` / `user_content` 取 `turn.prompt` 否则文档 `/prompt`，缺省为 `""`。

`/metric` 与 `/observation` 是整对象/整数组复制。因此
`prompt_tokens_len`、`completion_tokens_len`、`llm_infer_ms`、`env_action_ms`、
`stop_reason` 及未知 metric 键都在 `/metrics` 内；observation 元素的 `type`、
`tool_use_id`、`id`、`content`、`aggregated_output` 及其余键都在对应
`/observation/results/{o}` 对象内。这些子键不再单独占一行权威映射。

空 `tools` 使 `/tool_calls` 缺省（不是空数组）。空 `observation` 使 `/observation`
缺省。空字符串 `reasoning_content` 使 `/reason` 缺省。空 `error`、空 `artifacts`、
`null` 的 `final_answer` / `solved_at` / `score` 使对应 `/task/result` 键缺省。

`name` 为空或缺失时 `/fn` 回退到同 call 的 `type`；`type` 的权威目标只是 `/kind`。
`args` 按 `input` → `arguments` → `function.arguments` → `{"command": command}` →
剩余 tool 对象（不含 `type`/`id`/`status`/`exit_code`）选择；因此 `/args/name` 等扁平键只在既无 `input`
也无 `arguments` / `command` 时出现，不是 `name` 的权威目标。`name` / `input` / `arguments` /
`function` / `command` / `aggregated_output` / `type` / `status` / `exit_code` 只要在源对象上出现，就视为已消费，即使这次选择没有用到该键也不
写入残差。缺 `id` 时 Storyline `/tcid` 为 `step-{step_id}-tool-{index}`。`assistant_content.tool_calls` 上的同一组键同样已消费，不另写残差。

### 派生身份（不是字段拷贝）

Storyline 要求每份文档有非空 `/session`。ACTF 没有 session 字段。`/session` 由
`task_id` 与 attempt key 计算，不得再写进权威映射表：

```text
/session :=
    task_id                         当 attempts 恰有 1 个
    "{task_id}#attempt-{a}"         当 attempts 多于 1 个
```

残差文档键同样由 `task_id` 赋值，但它不是 Storyline 领域字段，只用来把同一 ACTF
文档拆出的 N 条 Storyline 在导出时合并：

```text
/unknown_fields/sources/actf/source_document_id := task_id
```

`/origin/document_id` 对 ACTF 保持缺省。同源恢复用 `/run` 还原 `task_id`，用
`/attempt_id` 还原 attempt map key，用上述 `source_document_id` 校验切片来自同一文档。

没有源 pointer 的常量：

| Storyline | 值 |
| --- | --- |
| `/schema_version` | `"storyline/v1"` |
| `/origin/format` | `"actf"` |
| `/agent/id` | `"actf-agent"` |
| `/agent/name` | `"ACTF Agent"` |
| `/turns/{t}/src` | `"agent"` |
| `/turns/{t}/nllm` | `1` |
| `/turns/{t}/kind` | `"autonomous"`（该 step 的 `tools` 非空）；否则缺省 |

### 便利提升

权威字段已经保存完整值。下列复制只服务 Storyline hub 顶栏或 ATIF 对齐别名；缺失或
条件不满足时不写，不得替代权威字段。

| 源（已在权威表中） | 额外写入 | 条件 |
| --- | --- | --- |
| `/task/result/task_correct` | `/final_metrics/task_correct` | 有值 |
| `/task/result/correct` | `/final_metrics/correct` | 有值 |
| `/task/result/status` | `/final_metrics/status` | 有值 |
| `/task/result/score` | `/final_metrics/score` | 有值 |
| `/task/result/max_score` | `/final_metrics/max_score` | 有值 |
| `/metrics/llm_infer_ms` | `/turns/{t}/latency_ms` | 值为 number |
| `/metrics/env_action_ms` | `/turns/{t}/tool_calls/0/duration_ms` | 值为 number，且该 step 恰有 1 个 tool call |
| `results/{o}/tool_use_id` | `results/{o}/source_call_id` | 非空；优先于 `id` |
| `results/{o}/id` | `results/{o}/source_call_id` | 非空，且没有 `tool_use_id` |
| `results/{o}/aggregated_output` | `results/{o}/content` | 有 `aggregated_output`；否则才用已有 `content` |

### 残差

未进入权威字段的键按原 pointer 写入
`/unknown_fields/sources/actf/fields/{E(P)}`。`task_id`、`correct`、`attempts` 容器、
已映射的 attempt `correct`/`score`/`max_score`/`status`/`analysis_result`/`final_answer`/`ground_truth`/`error`/`artifacts`/`extra`/`meta`/`trajectory.steps`，
根上的 `category`/`k`/`attempts_tried`/`solved_at`/`retry_count`/`retry_counts`，
trajectory 的 `started_at`/`finished_at`，step 的 `finished_at`、`system_prompt`、`user_content`，
以及 tool 的 `id`/`name`/`input`/`command`/`aggregated_output`/`type`/`status`/`exit_code` 不再作为残差保存。

| ACTF JSON Pointer |
| --- |
| `/{other-root-key}` |
| `/attempts/{a}/{other-attempt-key}` |
| `/attempts/{a}/trajectory/{other-trajectory-key}` |
| `/attempts/{a}/trajectory/steps/{s}/{other-step-key}` |
| `/attempts/{a}/trajectory/steps/{s}/assistant_content/{other-assistant-key}` |
| `/attempts/{a}/trajectory/steps/{s}/tools/{c}/{other-tool-key}` |
| `/attempts/{a}/trajectory/steps/{s}/assistant_content/tool_calls/{c}/{other-tool-key}` |

未知 root 残差复制到该 document 产生的每一条 Storyline；同源恢复时这些副本必须一致。
跨格式转换通过 version-1 `_storyline` envelope 携带外来 unknown fields。

## 保真边界

ACTF → Storyline → 三表 Lance → Storyline → ACTF 保证规范化 JSON 数据模型级语义一致：
未知键及其值（包括 `null`）、嵌套值、数组顺序和 attempt 分组均保留。已知字段的
missing/显式 `null` 会按 Storyline 语义规范化；源文件空白、缩进和对象键顺序不属于保真
边界。没有 ACTF source unknown fields 的普通 Storyline 可以导出为结构合法的单 attempt ACTF，
但这是有定义的合成转换，不宣称还原某个原始 ACTF 文件。`unknown_key_counts` 由
`unknown_fields` 确定性重算，没有独立源 pointer。

## Amendment history

| Date | Change |
| --- | --- |
| 2026-08-23 | `task.result` 强类型只保留通用评测字段；ACTF 私货进 extra，旧 Storyline JSON 同级键不得静默丢弃。OpenClaw 事件数组标为 ACTF 专属有损入口，不是独立 DocumentFormat。 |
| 2026-08-22 | 主映射改为每个源 pointer 恰好一个权威目标。`/session` 与 `source_document_id` 从 `/task_id` 拆出为派生身份；`kind`、`/fn` 回退、`latency_ms` / `duration_ms` / `source_call_id` / 规范化 `content` 改为便利提升。基数用语不再把 Storyline 文档叫成 run。 |
| 2026-08-22 | 评测/预算、文档与 step 结束时间、tool `type`/`status`/`exit_code` 进入 `/task`、`/started_at`、`/finished_at`、`tool_calls[].kind`/`response`；`task_correct`/`correct`/`status`/`score` 提升到 `/final_metrics`。 |
| 2026-08-22 | `system_prompt` / `user_content` 进入文档 `/prompt` 与 turn `/prompt`；不再作为残差。 |
| 2026-08-22 | `assistant_content.content` / `reasoning_content`、`system_prompt` / `user_content`、attempt `error` / `status` 接受 `null`，与空字符串同一规范化。 |
| 2026-08-22 | observation `type` 可选；仅有 `content` 的观察合法。合成导出仅在存在 tool 引用时补 `type=tool_result`。 |
| 2026-08-22 | `tools` 为空时回退 `assistant_content.tool_calls`；接受 OpenAI `function` 形状。 |
| 2026-08-22 | attempt `ground_truth` 接受任意 JSON（含 `{checklist_path}` 对象），权威目标仍是 `/task/result/ground_truth`。 |
| 2026-08-22 | tool `type`/`id` 可选；`{name,arguments}` 映射 `/fn`/`/args`，缺 `id` 合成 `step-{step_id}-tool-{index}`。attempt `status` 可空。 |
| 2026-08-22 | attempt `extra`/`meta` 进入文档 `/extra`/`/meta`；`max_score` 进入 `/task/result/max_score`。 |
