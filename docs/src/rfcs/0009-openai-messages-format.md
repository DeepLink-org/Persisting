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
`{last-user}`、`{output}`、`{context-message}` 与 `{tool-message}` 表示按上一节选中的
message：`{output}` 是有效 `/response`，否则是 `messages` 里选中的 assistant。

映射分成四类，禁止把不同角色的槽写进同一格：

| 类别 | 含义 | 每个源 pointer 的目标数 |
| --- | --- | --- |
| 权威字段 | OpenAI 值进入一个 Storyline 领域字段；同源导出从该字段还原 | 恰好 1 |
| 派生身份 | 由一个或多个源值计算，不是字段拷贝 | 公式，见下 |
| 便利提升 | 在权威字段之外再复制到 hub 顶栏或别名 | 额外 0 或 1，不替代权威字段 |
| 残差 | Storyline 没有对应领域字段，按原 pointer 进入 `unknown_fields` | 1（残差槽） |

`P` 表示左侧命中的完整源 pointer；`E(P)` 表示把整个 `P` 作为 `fields` 对象 key 后再做
一次 RFC 6901 token 转义。例如 `/session_steps/0/vendor_row` 保存在
`/unknown_fields/sources/openai-msg/fields/~1session_steps~10~1vendor_row`。

基数：

```text
OpenAI 文件              1 ──► N Storyline 文档（按 session_id 分组）
OpenAI row               1 ──► 0 或 1 条 request turn + 1 条 response turn
第一行 last-user 之前的
  system/user/assistant    1 ──► 1 条 copied context turn
OpenAI tool message      1 ──► 1 条 observation result
OpenAI tool call         1 ──► 1 条 Storyline tool_call
```

`meta_json` 与 `meta_json.env_state` 可以是 object，也可以是编码 object 的 JSON string；
表中的子 pointer 表示解码后的逻辑路径。无法解码时整个值进入残差。

### 权威字段（1:1）

| OpenAI Messages JSON Pointer | Storyline JSON Pointer |
| --- | --- |
| `/session_steps/{r}/session_id` | `/session` |
| `/session_steps/{r}/created_at` | `/turns/{response-t}/ts` |
| `/session_steps/0/messages/{context-message}/content` | `/turns/{t}/msg` |
| `/session_steps/{r}/messages/{last-user}/content` | `/turns/{request-t}/msg` |
| `/session_steps/{r}/messages/{tool-message}/tool_call_id` | `/turns/{response-t}/observation/results/{o}/source_call_id` |
| `/session_steps/{r}/messages/{tool-message}/content` | `/turns/{response-t}/observation/results/{o}/content` |
| `/session_steps/{r}/{output}/content` | `/turns/{response-t}/msg` |
| `/session_steps/{r}/{output}/reasoning_content` | `/turns/{response-t}/reason` |
| `/session_steps/{r}/{output}/tool_calls/{c}/id` | `/turns/{response-t}/tool_calls/{c}/tcid` |
| `/session_steps/{r}/{output}/tool_calls/{c}/function/name` | `/turns/{response-t}/tool_calls/{c}/fn` |
| `/session_steps/{r}/{output}/tool_calls/{c}/function/arguments` | `/turns/{response-t}/tool_calls/{c}/args` |
| `/session_steps/0/messages/{context-message}/tool_calls/{c}/id` | `/turns/{t}/tool_calls/{c}/tcid` |
| `/session_steps/0/messages/{context-message}/tool_calls/{c}/function/name` | `/turns/{t}/tool_calls/{c}/fn` |
| `/session_steps/0/messages/{context-message}/tool_calls/{c}/function/arguments` | `/turns/{t}/tool_calls/{c}/args` |
| `/session_steps/{r}/env_name` | `/task/env/name` |
| `/session_steps/{r}/meta_json/env_state/endpoint` | `/task/env/endpoint` |
| `/session_steps/{r}/dataset_type` | `/task/env/state/dataset_type` |
| `/session_steps/{r}/dt` | `/task/env/state/dt` |
| `/session_steps/{r}/meta_json/group_id` | `/task/env/state/group_id` |
| `/session_steps/{r}/meta_json/env_state/redaction_policy` | `/task/env/state/redaction_policy` |
| `/session_steps/{r}/meta_json/env_state/upstream_base_url` | `/task/env/state/upstream_base_url` |
| `/session_steps/{r}/meta_json/env_state/weight_version` | `/task/env/state/weight_version` |
| `/session_steps/{r}/id` | `/turns/{response-t}/env/id` |
| `/session_steps/{r}/meta_json/env_state/event_type` | `/turns/{response-t}/env/event_type` |
| `/session_steps/{r}/meta_json/env_state/request_id` | `/turns/{response-t}/env/request_id` |

已知 metric 键组成 response turn 上的一个 `/metrics` 对象，不再为每个键单列一行：

| 源 | 进入 `/turns/{response-t}/metrics` 的键 |
| --- | --- |
| `/session_steps/{r}/reward`、`/session_steps/{r}/step_reward`、`/session_steps/{r}/is_terminal`、`/session_steps/{r}/is_truncated`、`/session_steps/{r}/is_session_completed`、`/session_steps/{r}/is_trainable` | 同名 |
| `/session_steps/{r}/meta_json/env_state/{metric}` | 同名；`{metric}` 为 `prompt_tokens`、`completion_tokens`、`total_tokens`、`request_bytes`、`response_bytes`、`output_bytes`、`output_chunk_count`、`finish_reason`、`status_code`、`retry_count`、`upstream_latency_ms`、`gateway_overhead_ms`、`total_latency_ms`、`ttft_ms`、`truncate_reason`、`error_type`、`error_text`、`client_cancelled`、`upstream_cancelled`、`synthetic_stop`、`is_truncated`、`is_session_completed`、`max_steps`、`is_stream`、`payload_sampled`、`created_at`、`completed_at` |

row 顶层与 `env_state` 同名时，row 顶层值写入 `/metrics`，`env_state` 里的同名键视为已消费。
`function.arguments` 若为合法 JSON string，权威 `args` 是解析后的 JSON value，否则保留原
string。没有结构化 `tool_calls` 时，`{output}/content` 仍只映射到 `/msg`；其中受支持的
`<tool_call>` / `<function=...>` 标记另外**派生** `/tool_calls`，不是 `content` 的第二
个权威目标。`{output}/refusal` 仅在 `content` 没有有效值时回退写入 `/msg`，此时 `refusal`
视为已消费；若与有效 `content` 同时存在，`refusal` 进残差。

session-stable 的 `/task/env` 键取该 session **第一个非空值**；后续 row 上相等的值视为已消费
别名；后续不相等的值写入该 response turn 的 `/env`（含 `state` 浅 delta），导入不失败。
`id` / `event_type` / `request_id` 只写 response turn `/env`，不提升到 `/task/env`。copied
context turns 不写 `env`。`env_state` 里已进入 `/metrics` 的键（tokens、latency、
`status_code`、`finish_reason`、`created_at`/`completed_at` 等）不写入 `env`。row
`created_at` 不提升为文档 `/started_at`。

### 派生身份（不是字段拷贝）

```text
{request-t}  := context_count + 2 × step_id - 1     当该 row 有 last-user
{response-t} := context_count + 2 × step_id
/run         := session 内第一个非空 run_id → run_bucket → job_id
/agent/id    := 第一个非空 agent_id → meta_json.source → 首个 model → "openai-import"
/agent/name  := /agent/id
/agent/model := session 内第一个非空 agent_model → llm_model
/turns/{response-t}/model := 该 row 的 agent_model → llm_model
```

未选中的 `llm_model` 仅当等于已选 model 时才消费，否则进残差。

`context_count` 是第一行实际接纳的 context turns 数，不一定等于原 message 数。`step_id`
只用于计算 turn id，不拷贝到两个 `/id`。同一 session 后续 row 的 `/run` 候选必须与已选
值一致，否则导入失败。

没有 JSON 源 pointer 的常量与文件身份：

| Storyline | 值 |
| --- | --- |
| `/schema_version` | `"storyline/v1"` |
| `/origin/format` | `"openai-msg"` |
| `/origin/document_id` | 源文件相对路径 |
| `/unknown_fields/sources/openai-msg/source_document_id` | 同上；残差文档键，用来把同一文件拆出的 N 条 Storyline 再拼回去 |
| `/origin/schema_version` | 缺省 |
| context `/src` | `system → system`、`user → user`、`assistant → agent` |
| context `/copied` | `true` |
| context `/kind` | 有 tool calls 时 `"autonomous"`，assistant 为 `"llm.response"`，user 为 `"llm.request"`，system 为 `"context"` |
| request `/src`、`/kind` | `"user"`、`"llm.request"` |
| response `/src`、`/nllm` | `"agent"`、`1` |
| response `/kind` | 有 tool calls 时 `"autonomous"`，否则 `"llm.response"` |

`env_id`、`env_state.session_id` 仅当等于 `/session` 时视为冗余别名并消费；
`env_state.requested_model` 仅当等于该 row 已选 model 时消费；`env_state.llm_step_index`
仅当等于该 row `step_id` 时消费。不一致值进入残差，不改写 `/session`、`/model` 或 turn id。

tool-role message 的 `role` 只用于选出 observation，不映射到 result 对象。未选中的
assistant / 历史 messages 副本不生成新 turn。

### 便利提升

| 源（已在权威表或派生结果中） | 额外写入 | 条件 |
| --- | --- | --- |
| `/turns/{response-t}/ts` | `/turns/{request-t}/ts` | 该 row 有 request turn |
| `/turns/{response-t}/metrics` | `/final_metrics` | 只复制 session **最后一个** response turn |
| `/metrics/total_latency_ms` | `/turns/{response-t}/latency_ms` | 值为 number |
| `/metrics/ttft_ms` | `/turns/{response-t}/ttft_ms` | 值为 number |

### 残差

未进入权威字段且未被当作冗余别名/结构判别/空值消费的键，按原 pointer 写入
`/unknown_fields/sources/openai-msg/fields/{E(P)}`。已消费、不再作为残差保存的包括：
`session_id`、`step_id`、`created_at`、已选 run/agent/model 键、row 顶层与 `env_state`
中已进入 `/metrics` 的键、已进入 `/task/env` 或 response `/env` 的 env 键、选中 message 的 `role`/`content`、作为 output 消费的
`refusal`、string/null 的选中 `reasoning_content`、有效 tool call 的 `id` /
`function.name` / `function.arguments`、以及 `type="function"`。其它 `type` 值进入残差。

| OpenAI Messages JSON Pointer |
| --- |
| `/{unmapped-root}` |
| `/session_steps/{r}/{unmapped-row}` |
| `/session_steps/{r}/messages/{m}/{unmapped-message}` |
| `/session_steps/{r}/response/{unmapped-message}` |
| `/session_steps/{r}/messages/{m}/tool_calls/{c}/{unmapped-call}` |
| `/session_steps/{r}/response/tool_calls/{c}/{unmapped-call}` |
| `/session_steps/{r}/messages/{m}/tool_calls/{c}/function/{unmapped-function}` |
| `/session_steps/{r}/response/tool_calls/{c}/function/{unmapped-function}` |
| `/session_steps/{r}/meta_json/{unmapped-meta}` |
| `/session_steps/{r}/meta_json/env_state/{unmapped-env}` |

后续 row 里重复的历史 messages 视为 context 副本：已识别的 `role`/`content` 被规范化掉，
不重复生成 turns，也不进入残差。只有每行最后一个 user、选中的 assistant output 和
tool-role results 产生新语义。未选中 assistant 副本中的 string/null `reasoning_content`，
以及没有有效 content 时的非空 `refusal`，按副本规范化。已映射 message 的 `name`、
`refusal`、`tool_call_id`、`tool_calls` 中的 `null` 或空容器，以及 row 的
`blob_manifest`、`chosen_response`、`rejected_response`、`ground_truth_answer`、
`reference_answer` 中的 `null` 或空容器，按缺失值规范化；非空且未命中权威语义的值进入
残差。

同源恢复按原 pointer 写回。导出时若没有 openai-msg residual，合成行把 `/run` 写成
`job_id`、把 `/agent/id` 写成 `agent_id`、把 model 写成 `agent_model`，不宣称还原
`run_id` / `run_bucket` / `llm_model` 的原始键名。外来格式 residual 通过 version-1
`_storyline` envelope 携带。`unknown_key_counts` 由 `unknown_fields` 确定性重算，没有
独立源 pointer。

## 保真边界

同源 roundtrip 保留进入 Storyline 语义或 `unknown_fields` 的 JSON value、数组顺序与
session 分组。已知空值、重复历史 context、`type="function"` 结构判别，以及
`refusal` 在无 content 时并入 `/msg`，按上节规范化。文件空白、缩进、object key 顺序
以及顶层数组与 `session_steps` envelope 的原始排版不属于保真边界。

## Amendment history

| Date | Change |
| --- | --- |
| 2026-08-22 | 按 RFC-0004 的映射类别重写：每个源 pointer 恰好一个权威目标。`step_id`、`/run`、`/agent/id`、turn id 改为派生公式；`created_at` 只权威写入 response `/ts`；metric 收成 `/metrics` 对象并以 `/final_metrics` 为最后一 turn 的提升；`type`/`role`/`env_id` 等不再写成 1 到多字段拷贝。 |
| 2026-08-22 | session-stable env 键进入 `/task/env`；row `id`、`event_type`、`request_id` 进入 response turn `/env`。 |
