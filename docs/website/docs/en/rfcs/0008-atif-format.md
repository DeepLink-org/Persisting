# RFC-0008: ATIF v1.7 轨迹格式

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Format** | `atif` |
| **Date** | 2026-08-21 |
| **Component** | `persisting-pchronicle` |
| **Implements** | `crates/persisting-pchronicle/src/atif.rs` · `crates/persisting-pchronicle/src/convert/atif.rs` |
| **Upstream** | [Harbor RFC-0001: Agent Trajectory Interchange Format](https://github.com/harbor-framework/harbor/blob/main/rfcs/0001-trajectory-format.md) |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [RFC-0004 ACTF](0004-actf-format.md) · [RFC-0009 OpenAI Messages](0009-openai-messages-format.md) |

## 摘要

ATIF 是 pChronicle 支持的外部 JSON 轨迹交换格式。pChronicle 以 `ATIF-v1.7` 作为规范
输出版本，将每个 trajectory node 规范化为一份 Storyline；内嵌
`subagent_trajectories` 递归展开为独立 Storyline，并通过 `parent` 与 `children` 外链。
Storyline wire schema 由 [RFC-0001](0001-storyline-format.md) 定义，本 RFC 负责 ATIF
数据模型、校验边界及完整字段映射。

文件 reader 接受单个 ATIF object、ATIF object 数组以及 JSONL/NDJSON；下文 pointer
均以单个 trajectory object 为根。文件容器只影响批处理，不改变字段映射。

## JSON 数据模型

```text
AtifTrajectory
├── schema_version: string
├── session_id / trajectory_id: string?
├── agent: AtifAgent
├── steps: AtifStep[]
├── notes / continued_trajectory_ref: string?
├── final_metrics / extra: any?
└── subagent_trajectories: AtifTrajectory[]?

AtifAgent
├── name / version: string
├── model_name: string?
└── tool_definitions / extra: any?

AtifStep
├── step_id: integer
├── timestamp: RFC3339 string?
├── source: string
├── message: any
├── model_name / reasoning_content: string?
├── reasoning_effort: any?
├── tool_calls: AtifToolCall[]?
├── observation: { results: any[] }?
├── metrics / extra: any?
├── llm_call_count: integer?
└── is_copied_context: boolean?

AtifToolCall
├── tool_call_id / function_name: string
├── arguments: any
└── result / extra: any?
```

以上每个拥有字段的 object 都允许额外 key。额外 key 不扩张 Storyline schema，而按本 RFC
映射到 `unknown_fields`。

## 校验约束

- 根 trajectory 必须至少有一个非空 `session_id` 或 `trajectory_id`；内嵌 trajectory
  必须有非空且在整棵树中唯一的 `trajectory_id`。
- `agent.name` 与 `agent.version` 必须非空。
- 每个 trajectory node 内的 `step_id` 必须从 1 开始且唯一；数组顺序具有语义，不按 id
  重排。
- 每个 trajectory node 内的 `tool_call_id` 必须非空且全局唯一。
- 非空 `timestamp` 必须是可解析的 RFC3339 字符串。
- pChronicle 规范导出写 `ATIF-v1.7`；导入保留源 `schema_version` 到 Storyline origin。

## ATIF → Storyline JSON Pointer 映射 {#atif-storyline-json-pointer-mapping}

本节是 ATIF 到 Storyline 字段映射的权威定义。指针遵循 RFC 6901；`{s}`、`{c}` 和
`{t}` 分别表示源 step、tool call 与目标 turn 的数组下标。代入实际 token 后即为普通
JSON Pointer。

`P` 表示左侧命中的完整源 pointer；`E(P)` 表示把整个 `P` 作为 `fields` 对象 key 后再做
一次 RFC 6901 token 转义。所有输出都生成
`/schema_version = "storyline/v1"`、`/origin/format = "atif"`，并从
`/unknown_fields` 计算 `/unknown_key_counts`；这些值没有源 pointer，故不列入表。

| ATIF JSON Pointer | Storyline JSON Pointer |
| --- | --- |
| `/schema_version` | `/origin/schema_version` |
| `/session_id` | `/session` |
| `/trajectory_id` | `/trajectory`<br>`/session` |
| `/agent/name` | `/agent/id`<br>`/agent/name` |
| `/agent/version` | `/agent/ver` |
| `/agent/model_name` | `/agent/model` |
| `/agent/tool_definitions` | `/agent/tools` |
| `/agent/extra` | `/agent/extra` |
| `/notes` | `/notes` |
| `/final_metrics` | `/final_metrics` |
| `/continued_trajectory_ref` | `/continued_trajectory_ref` |
| `/extra` | `/extra` |
| `/steps/{s}/step_id` | `/turns/{t}/id` |
| `/steps/{s}/timestamp` | `/turns/{t}/ts` |
| `/steps/{s}/source` | `/turns/{t}/src`<br>`/turns/{t}/kind` |
| `/steps/{s}/message` | `/turns/{t}/msg` |
| `/steps/{s}/model_name` | `/turns/{t}/model` |
| `/steps/{s}/reasoning_effort` | `/turns/{t}/effort` |
| `/steps/{s}/reasoning_content` | `/turns/{t}/reason` |
| `/steps/{s}/tool_calls` | `/turns/{t}/tool_calls`<br>`/turns/{t}/kind` |
| `/steps/{s}/tool_calls/{c}/tool_call_id` | `/turns/{t}/tool_calls/{c}/tcid` |
| `/steps/{s}/tool_calls/{c}/function_name` | `/turns/{t}/tool_calls/{c}/fn` |
| `/steps/{s}/tool_calls/{c}/arguments` | `/turns/{t}/tool_calls/{c}/args` |
| `/steps/{s}/tool_calls/{c}/result` | `/turns/{t}/tool_calls/{c}/result` |
| `/steps/{s}/tool_calls/{c}/extra` | `/turns/{t}/tool_calls/{c}/extra` |
| `/steps/{s}/tool_calls/{c}/extra/duration_ms` | `/turns/{t}/tool_calls/{c}/extra/duration_ms`<br>`/turns/{t}/tool_calls/{c}/duration_ms` |
| `/steps/{s}/observation/results` | `/turns/{t}/observation/results` |
| `/steps/{s}/metrics` | `/turns/{t}/metrics` |
| `/steps/{s}/metrics/latency_ms` | `/turns/{t}/metrics/latency_ms`<br>`/turns/{t}/latency_ms` |
| `/steps/{s}/metrics/elapsed_ms` | `/turns/{t}/metrics/elapsed_ms`<br>`/turns/{t}/latency_ms` |
| `/steps/{s}/metrics/duration_ms` | `/turns/{t}/metrics/duration_ms`<br>`/turns/{t}/latency_ms` |
| `/steps/{s}/metrics/ttft_ms` | `/turns/{t}/metrics/ttft_ms`<br>`/turns/{t}/ttft_ms` |
| `/steps/{s}/extra` | `/turns/{t}/extra` |
| `/steps/{s}/llm_call_count` | `/turns/{t}/nllm` |
| `/steps/{s}/is_copied_context` | `/turns/{t}/copied` |
| `/subagent_trajectories/{c}` | `""` |
| `/subagent_trajectories/{c}/session_id` | `/session` |
| `/subagent_trajectories/{c}/trajectory_id` | `/children/{c}`<br>`/trajectory`<br>`/session` |
| `/session_id`<br>`/trajectory_id` | `/parent/psid` |
| `/{unmapped-root}`<br>`/agent/{unmapped-agent}`<br>`/steps/{s}/{unmapped-step}`<br>`/steps/{s}/tool_calls/{c}/{unmapped-tool}`<br>`/steps/{s}/observation/{unmapped-observation}` | `/unknown_fields/sources/atif/fields/{E(P)}` |

条件和规范化规则：

- `/session` 按非空 `session_id → trajectory_id → 父 trajectory 的有效 session` 选择；
  根 trajectory 缺少前两者时导入失败。`/agent/id` 与 `/agent/name` 都取 `agent.name`。
- `/turns/{t}` 与 `/steps/{s}` 保持同一数组顺序。`kind` 通常省略，由 `src` 和
  `tool_calls` 推导；agent turn 含 tool calls 时显式为 `autonomous`，非标准 source
  显式为 `dialogue`。
- `/latency_ms` 按 `metrics.latency_ms → metrics.elapsed_ms → metrics.duration_ms` 选择，
  `/ttft_ms` 取 `metrics.ttft_ms`；数值转换为整数毫秒。完整 metrics value 仍原样保留。
- `tool_calls.extra.duration_ms` 提升到 call 顶层时，完整 `/extra` 仍保留，因此可以双向
  恢复。
- 父 Storyline `/children/{c}` 保存 child `trajectory_id`。child `/parent/psid` 取父
  `trajectory_id`，缺失时取父有效 `session`；`/parent/rel` 固定为 `spawn`。上表对每个
  `/subagent_trajectories/{c}` 递归适用：源 pointer 加 child 前缀，目标 pointer 从新
  Storyline 文档根重新开始。
- ATIF source document id 是去除 `_storyline` envelope、递归排序 object key 后对规范 JSON
  计算的 BLAKE3；它写入 `/unknown_fields/sources/atif/source_document_id`，没有直接源字段。
- 除已知 optional 字段的 `null`/missing 规范化外，不能映射到 Storyline 已知字段的值都
  进入 `unknown_fields`。同源恢复按原 pointer 写回；外来格式 residual 通过 version-1
  `_storyline` envelope 携带。

## 保真边界

ATIF → Storyline → Lance → Storyline → ATIF 保留 JSON 语义、数组顺序、未知 key/value、
嵌套 subagent、trajectory/session 身份及有效 RFC3339 原文。已知 optional 字段的显式
`null` 与 missing 归一为同一缺失语义；源文件空白、缩进、object key 顺序和输入容器排版
不属于保真边界。
