# RFC-0009: OpenAI Messages 轨迹语料格式

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Format** | `openai-msg`（CLI alias: `openai-messages`） |
| **Date** | 2026-08-21 |
| **Component** | `persisting-pchronicle` |
| **Implements** | `crates/persisting-pchronicle/src/formats/openai_corpus.rs` |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [RFC-0004 ACTF](0004-actf-format.md) · [RFC-0008 ATIF](0008-atif-format.md) |

## 摘要

OpenAI Messages 是 pChronicle 面向训练、评测和回放语料支持的 row-based JSON 格式。
reader 接受顶层 row 数组或 `{ "session_steps": [...] }` envelope，按 `session_id` 分组，
再把每个 source step 规范化为 request/response turns。Storyline wire schema 由
[RFC-0001](0001-storyline-format.md) 定义；本 RFC 是输入形态、选择规则与字段映射的
权威文档。

## 接受的数据形态

```text
OpenaiCorpus = OpenaiRow[] | { session_steps: OpenaiRow[], ...root fields }

OpenaiRow
├── session_id: non-empty string
├── step_id: positive integer
├── messages: OpenaiMessage[]?
├── response: OpenaiMessage?
├── created_at: RFC3339 string | Unix epoch seconds?
├── agent_id / agent_model / llm_model: string?
├── run_id / run_bucket / job_id: string?
├── meta_json: object | JSON-encoded object string?
└── arbitrary row fields

OpenaiMessage
├── role: string?
├── content / refusal: any?
├── reasoning_content: string?
├── name / tool_call_id: string?
└── tool_calls: OpenaiToolCall[]?

OpenaiToolCall
├── id / type: string?
└── function: { name: string?, arguments: any?, ... }
```

每个 row 必须有非空 `session_id`、正整数且在 session 内唯一的 `step_id`，并且必须能从
`response` 或 `messages` 选出一个有效 assistant output。相同 session 中非空 run identity
必须一致；turn id 计算不得溢出。未知字段不扩张 Storyline schema，按本 RFC 进入
`unknown_fields`。

## Turn 构造与选择

第一行最后一条 user message 之前、role 为 `system`、`user` 或 `assistant` 的 message
形成 context turns，并写 `/copied = true`。每行最后一条 user message 形成 request turn。
assistant output 优先选择包含有效输出的 `/response`；否则从 `messages` 末尾向前选择第一条
包含非空 content、refusal 或 tool calls 的 assistant message。row 中 role 为 `tool` 且有
非空 `tool_call_id` 的 message 形成该 response turn 的 observation results。

## OpenAI Messages → Storyline JSON Pointer 映射 {#openai-storyline-json-pointer-mapping}

本节是 OpenAI Messages 到 Storyline 字段映射的权威定义。顶层数组在映射和 unknown-field
记录中规范化为 `/session_steps/{r}`。指针遵循 RFC 6901；`{r}`、`{m}`、`{c}`、`{o}`
和 `{t}` 分别表示 row、message、tool call、observation result 和目标 turn 下标。
`{last-user}`、`{selected-assistant}`、`{context-message}` 与 `{tool-message}` 表示按上一节
选中的 message 下标。

`P` 表示左侧命中的完整源 pointer；`E(P)` 表示把整个 `P` 作为 `fields` 对象 key 后再做
一次 RFC 6901 token 转义。例如 `/session_steps/0/env_name` 保存在
`/unknown_fields/sources/openai-msg/fields/~1session_steps~10~1env_name`。所有输出都生成
`/schema_version = "storyline/v1"`、`/origin/format = "openai-msg"`、
`/origin/document_id = source relative path`，并从 `/unknown_fields` 计算
`/unknown_key_counts`；这些值没有源 pointer，故不列入表。

| OpenAI Messages JSON Pointer | Storyline JSON Pointer |
| --- | --- |
| `/session_steps/{r}/session_id` | `/session` |
| `/session_steps/{r}/step_id` | `/turns/{request-t}/id`<br>`/turns/{response-t}/id` |
| `/session_steps/{r}/created_at` | `/turns/{request-t}/ts`<br>`/turns/{response-t}/ts` |
| `/session_steps/{r}/run_id` | `/run` |
| `/session_steps/{r}/run_bucket` | `/run` |
| `/session_steps/{r}/job_id` | `/run` |
| `/session_steps/{r}/env_id` | `/session` |
| `/session_steps/{r}/agent_id` | `/agent/id`<br>`/agent/name` |
| `/session_steps/{r}/meta_json/source` | `/agent/id`<br>`/agent/name` |
| `/session_steps/{r}/agent_model` | `/agent/model`<br>`/turns/{response-t}/model` |
| `/session_steps/{r}/llm_model` | `/agent/model`<br>`/turns/{response-t}/model` |
| `/session_steps/{r}/meta_json/env_state/session_id` | `/session` |
| `/session_steps/{r}/meta_json/env_state/requested_model` | `/turns/{response-t}/model` |
| `/session_steps/{r}/meta_json/env_state/llm_step_index` | `/turns/{request-t}/id`<br>`/turns/{response-t}/id` |
| `/session_steps/0/messages/{context-message}/role` | `/turns/{t}/src`<br>`/turns/{t}/kind`<br>`/turns/{t}/copied` |
| `/session_steps/0/messages/{context-message}/content` | `/turns/{t}/msg` |
| `/session_steps/{r}/messages/{last-user}/role` | `/turns/{request-t}/src`<br>`/turns/{request-t}/kind` |
| `/session_steps/{r}/messages/{last-user}/content` | `/turns/{request-t}/msg` |
| `/session_steps/{r}/messages/{tool-message}/role` | `/turns/{response-t}/observation/results/{o}` |
| `/session_steps/{r}/messages/{tool-message}/tool_call_id` | `/turns/{response-t}/observation/results/{o}/source_call_id` |
| `/session_steps/{r}/messages/{tool-message}/content` | `/turns/{response-t}/observation/results/{o}/content` |
| `/session_steps/{r}/response/role`<br>`/session_steps/{r}/messages/{selected-assistant}/role` | `/turns/{response-t}/src`<br>`/turns/{response-t}/kind` |
| `/session_steps/{r}/response/content`<br>`/session_steps/{r}/messages/{selected-assistant}/content` | `/turns/{response-t}/msg`<br>`/turns/{response-t}/tool_calls/{c}` |
| `/session_steps/{r}/response/refusal`<br>`/session_steps/{r}/messages/{selected-assistant}/refusal` | `/turns/{response-t}/msg` |
| `/session_steps/{r}/response/reasoning_content`<br>`/session_steps/{r}/messages/{selected-assistant}/reasoning_content` | `/turns/{response-t}/reason` |
| `/session_steps/{r}/response/tool_calls/{c}/id`<br>`/session_steps/{r}/messages/{selected-assistant}/tool_calls/{c}/id`<br>`/session_steps/0/messages/{context-message}/tool_calls/{c}/id` | `/turns/{t}/tool_calls/{c}/tcid` |
| `/session_steps/{r}/response/tool_calls/{c}/type`<br>`/session_steps/{r}/messages/{selected-assistant}/tool_calls/{c}/type`<br>`/session_steps/0/messages/{context-message}/tool_calls/{c}/type` | `/turns/{t}/tool_calls/{c}`<br>`/unknown_fields/sources/openai-msg/fields/{E(P)}` |
| `/session_steps/{r}/response/tool_calls/{c}/function/name`<br>`/session_steps/{r}/messages/{selected-assistant}/tool_calls/{c}/function/name`<br>`/session_steps/0/messages/{context-message}/tool_calls/{c}/function/name` | `/turns/{t}/tool_calls/{c}/fn` |
| `/session_steps/{r}/response/tool_calls/{c}/function/arguments`<br>`/session_steps/{r}/messages/{selected-assistant}/tool_calls/{c}/function/arguments`<br>`/session_steps/0/messages/{context-message}/tool_calls/{c}/function/arguments` | `/turns/{t}/tool_calls/{c}/args` |
| `/session_steps/{r}/reward` | `/turns/{response-t}/metrics/reward`<br>`/final_metrics/reward` |
| `/session_steps/{r}/step_reward` | `/turns/{response-t}/metrics/step_reward`<br>`/final_metrics/step_reward` |
| `/session_steps/{r}/is_terminal` | `/turns/{response-t}/metrics/is_terminal`<br>`/final_metrics/is_terminal` |
| `/session_steps/{r}/is_truncated` | `/turns/{response-t}/metrics/is_truncated`<br>`/final_metrics/is_truncated` |
| `/session_steps/{r}/is_session_completed` | `/turns/{response-t}/metrics/is_session_completed`<br>`/final_metrics/is_session_completed` |
| `/session_steps/{r}/is_trainable` | `/turns/{response-t}/metrics/is_trainable`<br>`/final_metrics/is_trainable` |
| `/session_steps/{r}/meta_json/env_state/prompt_tokens` | `/turns/{response-t}/metrics/prompt_tokens`<br>`/final_metrics/prompt_tokens` |
| `/session_steps/{r}/meta_json/env_state/completion_tokens` | `/turns/{response-t}/metrics/completion_tokens`<br>`/final_metrics/completion_tokens` |
| `/session_steps/{r}/meta_json/env_state/total_tokens` | `/turns/{response-t}/metrics/total_tokens`<br>`/final_metrics/total_tokens` |
| `/session_steps/{r}/meta_json/env_state/request_bytes` | `/turns/{response-t}/metrics/request_bytes`<br>`/final_metrics/request_bytes` |
| `/session_steps/{r}/meta_json/env_state/response_bytes` | `/turns/{response-t}/metrics/response_bytes`<br>`/final_metrics/response_bytes` |
| `/session_steps/{r}/meta_json/env_state/output_bytes` | `/turns/{response-t}/metrics/output_bytes`<br>`/final_metrics/output_bytes` |
| `/session_steps/{r}/meta_json/env_state/output_chunk_count` | `/turns/{response-t}/metrics/output_chunk_count`<br>`/final_metrics/output_chunk_count` |
| `/session_steps/{r}/meta_json/env_state/finish_reason` | `/turns/{response-t}/metrics/finish_reason`<br>`/final_metrics/finish_reason` |
| `/session_steps/{r}/meta_json/env_state/status_code` | `/turns/{response-t}/metrics/status_code`<br>`/final_metrics/status_code` |
| `/session_steps/{r}/meta_json/env_state/retry_count` | `/turns/{response-t}/metrics/retry_count`<br>`/final_metrics/retry_count` |
| `/session_steps/{r}/meta_json/env_state/upstream_latency_ms` | `/turns/{response-t}/metrics/upstream_latency_ms`<br>`/final_metrics/upstream_latency_ms` |
| `/session_steps/{r}/meta_json/env_state/gateway_overhead_ms` | `/turns/{response-t}/metrics/gateway_overhead_ms`<br>`/final_metrics/gateway_overhead_ms` |
| `/session_steps/{r}/meta_json/env_state/total_latency_ms` | `/turns/{response-t}/metrics/total_latency_ms`<br>`/turns/{response-t}/latency_ms`<br>`/final_metrics/total_latency_ms` |
| `/session_steps/{r}/meta_json/env_state/ttft_ms` | `/turns/{response-t}/metrics/ttft_ms`<br>`/turns/{response-t}/ttft_ms`<br>`/final_metrics/ttft_ms` |
| `/session_steps/{r}/meta_json/env_state/truncate_reason` | `/turns/{response-t}/metrics/truncate_reason`<br>`/final_metrics/truncate_reason` |
| `/session_steps/{r}/meta_json/env_state/error_type` | `/turns/{response-t}/metrics/error_type`<br>`/final_metrics/error_type` |
| `/session_steps/{r}/meta_json/env_state/error_text` | `/turns/{response-t}/metrics/error_text`<br>`/final_metrics/error_text` |
| `/session_steps/{r}/meta_json/env_state/client_cancelled` | `/turns/{response-t}/metrics/client_cancelled`<br>`/final_metrics/client_cancelled` |
| `/session_steps/{r}/meta_json/env_state/upstream_cancelled` | `/turns/{response-t}/metrics/upstream_cancelled`<br>`/final_metrics/upstream_cancelled` |
| `/session_steps/{r}/meta_json/env_state/synthetic_stop` | `/turns/{response-t}/metrics/synthetic_stop`<br>`/final_metrics/synthetic_stop` |
| `/session_steps/{r}/meta_json/env_state/is_truncated` | `/turns/{response-t}/metrics/is_truncated`<br>`/final_metrics/is_truncated` |
| `/session_steps/{r}/meta_json/env_state/is_session_completed` | `/turns/{response-t}/metrics/is_session_completed`<br>`/final_metrics/is_session_completed` |
| `/session_steps/{r}/meta_json/env_state/max_steps` | `/turns/{response-t}/metrics/max_steps`<br>`/final_metrics/max_steps` |
| `/session_steps/{r}/meta_json/env_state/is_stream` | `/turns/{response-t}/metrics/is_stream`<br>`/final_metrics/is_stream` |
| `/session_steps/{r}/meta_json/env_state/payload_sampled` | `/turns/{response-t}/metrics/payload_sampled`<br>`/final_metrics/payload_sampled` |
| `/session_steps/{r}/meta_json/env_state/created_at` | `/turns/{response-t}/metrics/created_at`<br>`/final_metrics/created_at` |
| `/session_steps/{r}/meta_json/env_state/completed_at` | `/turns/{response-t}/metrics/completed_at`<br>`/final_metrics/completed_at` |
| `/{unmapped-root}`<br>`/session_steps/{r}/{unmapped-row}`<br>`/session_steps/{r}/messages/{m}/{unmapped-message}`<br>`/session_steps/{r}/response/{unmapped-message}`<br>`/session_steps/{r}/messages/{m}/tool_calls/{c}/{unmapped-call}`<br>`/session_steps/{r}/response/tool_calls/{c}/{unmapped-call}`<br>`/session_steps/{r}/messages/{m}/tool_calls/{c}/function/{unmapped-function}`<br>`/session_steps/{r}/response/tool_calls/{c}/function/{unmapped-function}`<br>`/session_steps/{r}/meta_json/{unmapped-meta}`<br>`/session_steps/{r}/meta_json/env_state/{unmapped-env}` | `/unknown_fields/sources/openai-msg/fields/{E(P)}` |

条件和规范化规则：

- `{request-t} = context_count + 2 × step_id - 1`，
  `{response-t} = context_count + 2 × step_id`。`context_count` 是第一行实际接纳的 context
  turns 数，不一定等于原 message 数。
- context role 映射为 `system → system`、`user → user`、`assistant → agent`；request 固定
  为 `src=user, kind=llm.request`，response 固定为 `src=agent`，有 tool calls 时
  `kind=autonomous`，否则为 `llm.response`。
- `/run` 按 `run_id → run_bucket → job_id` 选择 session 内第一个非空值；后续 row 必须
  一致。`/agent/id` 按 `agent_id → meta_json.source → 首个 model → openai-import` 选择。
  model 在每行按 `agent_model → llm_model` 选择，首个 model 同时写入 `/agent/model`。
- `env_id`、`env_state.session_id`、`env_state.requested_model` 和
  `env_state.llm_step_index` 只在与规范字段一致时视为冗余别名；不一致值进入
  `unknown_fields`。
- `meta_json` 和 `meta_json.env_state` 可以是 object，也可以是编码 object 的 JSON string；
  表中的子 pointer 表示解码后的逻辑路径。无法解码时整个值进入 `unknown_fields`。
- `function.arguments` 若为合法 JSON string，会解析成对应 JSON value；否则保留原 string。
  没有结构化 `tool_calls` 时，response content 中受支持的 `<tool_call>` 或
  `<function=...>` 标记还会派生 `/tool_calls`，原 content 仍写入 `/msg`。
- tool call 的 `type="function"` 是结构判别值，没有独立 Storyline 字段，会被规范化掉；
  其它 `type` 值不影响 id/name/arguments 映射，并额外进入 `unknown_fields`。
- `/final_metrics/*` 只复制 session 最后一个 response turn 的 metrics。row 顶层 metric 与
  env-state metric 同名时，row 顶层值优先。
- 已映射 message 的 `name`、`refusal`、`tool_call_id`、`tool_calls` 中的 `null` 或空容器，
  以及 row 的 `blob_manifest`、`chosen_response`、`rejected_response`、
  `ground_truth_answer`、`reference_answer` 中的 `null` 或空容器，按缺失值规范化；非空且
  未命中表中语义的值进入 `unknown_fields`。
- 后续 row 重复携带的历史 messages 被视为 context 副本，不重复生成 turns。当前实现会
  规范化掉这些副本中已识别的 role/content；只有每行最后一个 user、选中的 assistant
  output 和 tool-role results 产生新语义。只有选中的 assistant output 保留
  `reasoning_content`；未选中的 assistant 副本中的 string/null `reasoning_content`，以及
  没有有效 content 时的非空 `refusal`，也按副本规范化，不进入 `unknown_fields`。
- 除上述明确的结构判别、冗余副本和空值规范化外，不能映射到 Storyline 已知字段的值都
  进入 `unknown_fields`，同源恢复按原 pointer 写回。外来格式 residual 通过 version-1
  `_storyline` envelope 携带。

## 保真边界

同源 roundtrip 保留进入 Storyline 语义或 `unknown_fields` 的 JSON value、数组顺序与
session 分组。已知空值、重复历史 context 和结构判别字段按上节规范化；文件空白、缩进、
object key 顺序以及顶层数组与 `session_steps` envelope 的原始排版不属于保真边界。
