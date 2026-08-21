# pChronicle OpenAI Messages 字段映射设计

## 状态

本设计于 2026-08-20 在对话中确认。它定义 OpenAI Messages corpus 与
Storyline/ATIF 之间的字段映射，以及无法映射字段的 unknown-field 行为。

## 背景

当前 OpenAI Messages adapter 把两类数据都放进 `unknown_fields`：

1. Storyline/ATIF 确实没有正式字段承载的数据；
2. adapter 已经理解、甚至已经用于构造 Storyline，但为了恢复原始 JSON 仍保留的字段。

因此 `pchronicle import` 会把 `step_id`、`messages[].role`、
`messages[].content`、`tool_calls` 和若干状态字段报告为 unknown。用户无法从 warning
区分真正未映射的数据，也无法通过 SQL 查询其中一些本来可以规范化的字段。

本设计把规则收敛为一个简单原则：**有明确映射的字段写入 Storyline/ATIF；已知可选空值
视为未提供；其余字段写入 `unknown_fields`。**

## 目标

- 为 OpenAI Messages corpus 中可表达的字段建立明确映射。
- 让 `step_id`、消息、tool calls、状态和性能数据进入 Storyline/ATIF 正式字段。
- 让 warning 只报告没有映射规则的字段。
- 保持 OpenAI Messages 与 Storyline 之间的逻辑双向转换。
- 保留现有嵌入文本 tool-call 解析能力。
- 使用当前统一 `unknown_fields` 模型，不增加新的旁路状态或恢复协议。

## 非目标

- 不逐字节恢复原始 JSON。
- 不保留原始 row 顺序、滑动窗口边界、object key 顺序或 `null` 与 missing 的区别。
- 不为原始物理布局增加 consumed-path registry、row carrier 或路径重定位协议。
- 不把任意来源 metadata 塞进 `extra` 或伪装成 metric。
- 不从正文中的 `<think>` 标签推断 `reasoning_content`；只有显式来源字段才映射。
- 不修改 TTAS、Queue/Sampler、Search 或 `persisting-dlcapt`。

## 核心算法

adapter 取得一个可变来源 object，并按字段规则逐项 `remove`/`take`：

```text
读取字段
  ├─ 有明确映射规则        -> 写入 Storyline/ATIF
  ├─ 已知可选字段且为空    -> 忽略
  └─ 没有映射规则          -> 写入 unknown_fields
```

部分可映射的嵌套对象使用相同过程。例如，处理一个 message 时取走 `role`、`content`、
可映射的 `tool_calls` 和可关联的 `tool_call_id`，剩余 member 进入 unknown。处理
`meta_json.env_state` 时只取走白名单字段，剩余 member 进入 unknown。

不需要另一套字段判定表来猜测哪些路径“看起来 canonical”。映射函数本身就是唯一事实源：
成功取走的字段已映射，处理结束后剩余的字段未映射。

## 轨迹与 turn 构造

rows 按 `session_id` 分组，并在 session 内按正整数 `step_id` 排序。每个 session 生成一条
Storyline：

1. 只从第一行导入当前交互之前的 leading messages，作为 context turns；
2. 每一行只新增当前 user 和当前 agent 两个 turns；
3. 后续行重复携带的历史 messages 是已知快照表示，不重复生成 turns，也不作为 unknown；
4. 有有效 `response` 时使用 `response` 作为当前 agent 输出，否则使用 `messages` 中最后一个
   有效 assistant message。

设第一行 context turn 数为 `k`，来源 `step_id` 为 `n`：

- context turn IDs 为 `1..k`；
- 当前 user turn ID 为 `k + 2n - 1`；
- 当前 agent turn ID 为 `k + 2n`。

导出时使用同一公式反向计算 `step_id`。输入契约保证 session 内 step ID 可用于该计算；
adapter 不额外检查连续性、时间单调性或指标数值合理性。

## 字段映射

### 轨迹与 agent

| OpenAI 字段 | Storyline/ATIF 字段 | 规则 |
| --- | --- | --- |
| `session_id` | `session_id` | 一个 session 对应一条轨迹 |
| `job_id` / `run_id` / `run_bucket` | `run_id` | 按现有优先级选取 |
| `agent_id`，否则 `meta_json.source` | `agent.id` | 显式 `agent_id` 优先 |
| `agent_model` / `llm_model` | `agent.model_name`、agent turn `model_name` | session 级模型信息 |
| `step_id` | turn `id` | 使用 context 偏移公式 |
| `created_at` | 当前 user、agent turn `timestamp` | 导出时以 agent timestamp 生成 row 字段 |

以下字段作为等价别名处理：

- `env_id == session_id`；
- `meta_json.env_state.session_id == session_id`；
- `meta_json.env_state.requested_model == agent_model`；
- `meta_json.env_state.llm_step_index == step_id`。

值相等时字段已被理解并消费；值不相等时没有正式承载位置，因此进入 unknown。

### Messages 与 tool calls

| OpenAI 字段 | Storyline/ATIF 字段 | 规则 |
| --- | --- | --- |
| `role=system` | `source=system` | 作为 context turn |
| `role=user` | `source=user` | 保留完整 `content` JSON value |
| `role=assistant` | `source=agent` | 保留完整 `content` JSON value |
| `role=tool` | `observation` 或 tool-call `result` | 使用 `tool_call_id` 关联已有调用 |
| `tool_calls[].id` | `tool_call_id` | 非空字符串 |
| `tool_calls[].type=function` | tool-call 类型 | 已知 discriminator，消费但无需独立字段 |
| `tool_calls[].function.name` | `function_name` | 非空字符串 |
| `tool_calls[].function.arguments` | `arguments` | JSON 字符串可解析时转为 JSON value，否则保留字符串 |
| 显式 `reasoning_content` | `reasoning_content` | 不解析正文标签 |
| `refusal` | agent `message` | 仅在 `content` 为空时作为输出正文；导出规范化为 `content` |

第一行 leading context turns 标记 `is_copied_context=true`。标准结构化 tool calls 优先；
没有结构化调用时，继续使用当前嵌入文本 tool-call parser。无法关联的非空
`tool_call_id`、非空 `name` 等没有正式承载位置的字段进入 unknown。

### Metrics

行级字段映射到当前 agent turn 的 `metrics`：

- `reward`、`step_reward`；
- `is_terminal`、`is_truncated`、`is_session_completed`、`is_trainable`。

`meta_json.env_state` 只消费以下白名单字段：

- token/容量：`prompt_tokens`、`completion_tokens`、`total_tokens`、`request_bytes`、
  `response_bytes`、`output_bytes`、`output_chunk_count`；
- 性能：`upstream_latency_ms`、`gateway_overhead_ms`、`total_latency_ms`、`ttft_ms`、
  `retry_count`；
- 状态：`status_code`、`finish_reason`、`truncate_reason`、`error_type`、`error_text`、
  `client_cancelled`、`upstream_cancelled`、`synthetic_stop`、`is_truncated`、
  `is_session_completed`；
- 运行参数：`max_steps`、`is_stream`、`payload_sampled`、`created_at`、`completed_at`。

`total_latency_ms` 和 `ttft_ms` 同时投影到 Storyline 的专用 timing 字段。metrics 保留原始
JSON number，专用整数字段按现有数值转换规则计算。行级字段优先；env-state 中的同名字段
仅在行级值缺失时补充。同名值相等时 env-state 值作为冗余表示消费，值不等时 env-state
值进入 unknown。最后一个 agent turn 的 metrics 同时成为轨迹 `final_metrics`。

### 无正式映射的字段

下列非空字段没有合适的 Storyline/ATIF 正式字段，因此进入 unknown：

- `dataset_type`、`dt`、`env_name`、row `id`；
- `meta_json.group_id`、`request_id`、`event_type`；
- `endpoint`、`upstream_base_url`、`redaction_policy`、`weight_version`；
- 非空 `blob_manifest`；
- `chosen_response`、`rejected_response`、`ground_truth_answer`、`reference_answer`；
- 其他没有列入映射表或 metric 白名单的来源字段。

来源 schema 中已知可选字段的 `null`、空数组和空对象视为未提供，不进入 unknown，也不
产生 warning；这包括 `name`、`refusal`、`tool_call_id`、`tool_calls`、`blob_manifest` 和
几个 answer/preference 字段。`content` 仍作为完整 message value 映射，即使它为空。未知
厂商 key 不享受空值规则：adapter 不知道其空值是否具有语义，因此仍写入 unknown。

## 导出语义

OpenAI 导出由已映射的 Storyline 字段生成规范化 rows：

- `messages` 包含首行 context、此前规范交互以及当前 user；
- 当前 agent 固定写入 `response`；
- `step_id` 由 turn ID 反向计算；
- agent、timestamp、metrics 和 tool calls 使用上述映射的逆过程生成；
- 未映射数据继续由现有 `unknown_fields` 机制携带。

逻辑双向的判定是 OpenAI -> Storyline -> OpenAI -> Storyline 后，正式 Storyline 字段保持
一致；不要求中间 OpenAI JSON 与原输入物理相同。

## 错误行为

- 缺少非空 `session_id`、正整数 `step_id`、当前 user 或当前 agent output：输入无效。
- session 内重复 `step_id` 或冲突的 session 级 `run_id`：输入无效。
- 可选结构无法解析或形状不合法，例如畸形 `tool_calls` 或 `meta_json`：该完整来源值进入
  unknown 并产生 warning，不阻断其他可映射字段。
- 不增加额外的数据连续性、时间或数值正确性验证。

## 实现范围

主要修改 `crates/persisting-pchronicle/src/formats/openai_corpus.rs`：

- 用逐字段 take/remove 的映射流程替代“canonical key + recovery residual”混合判定；
- 增加 context turn 构造和 step-ID 偏移；
- 扩充状态与 env-state metric 映射；
- 更新反向编码；
- 更新该模块内的字段级单元测试。

CLI 侧只更新与 unknown warning 相关的测试。除非实现过程中发现现有公共 helper 缺少必要
能力，否则不改变 Storyline/ATIF wire schema、Lance schema 或通用 unknown-fields 模型。

## 测试策略

实现遵循 RED -> GREEN，使用小型代表性 fixture 覆盖：

1. `step_id` 映射为 context 偏移后的 turn IDs，并可反向计算；
2. `role`、`content`、`response`、`tool_calls`、tool results 映射到正确字段；
3. reward、状态和性能字段进入 metrics 与专用 timing 字段；
4. 已知可选空值不进入 unknown、不产生 warning；
5. 任意没有映射规则的字段完整进入 unknown；
6. 已映射字段不再出现在 unknown warning 中；
7. OpenAI -> Storyline -> OpenAI -> Storyline 的正式字段保持一致；
8. 更新原来断言 `step_id`、`role/content` 属于 unknown 的旧测试。

再使用 `data/cybergym_0729001.json` 做手动验收，预期：

- 8 条 trajectories；
- 964 个 turns，其中包含每个 session 的首行 context；
- 461 个 tool calls；
- warning 不含 `step_id`、`messages/*/role`、`messages/*/content`、
  `messages/*/tool_calls`、`response/*` 等已映射字段；
- `is_terminal`、`is_session_completed` 等字段可通过 SQL 查询；
- 真正没有映射规则的非空字段仍产生 warning。

定向验证范围为 pChronicle library 与 CLI：

```text
cargo test -p persisting-pchronicle openai
cargo test -p persisting-pchronicle-cli openai
cargo fmt -p persisting-pchronicle -p persisting-pchronicle-cli -- --check
cargo clippy -p persisting-pchronicle -p persisting-pchronicle-cli --all-targets -- -D warnings
```

其他子系统的现有失败不扩大本任务的验收范围。

## 验收标准

- 每个已声明映射的非空字段进入对应 Storyline/ATIF 正式字段；
- 已知可选空值不进入 unknown；
- 每个没有映射规则的字段进入 `unknown_fields`；
- warning 不再把已映射字段报告为 unknown；
- 当前 corpus 的轨迹、turn、tool-call 计数符合预期；
- 状态与性能字段可以通过 pChronicle SQL 查询；
- 逻辑双向测试、定向测试、格式检查和 Clippy 通过。
