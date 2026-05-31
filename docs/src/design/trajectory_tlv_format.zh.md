# 轨迹 Markdown 格式

会话 Markdown 是轨迹的**物化人读视图**：从 Vortex raw event log **materialize** 得到，或在 capture `-f md` 时由 **CaptureEngine live upsert** 增量更新。

> **Capture `-f md`**：Proxy 内 CaptureEngine 按 `call_id`+`role` **upsert** 块到对应 md（主 session → `run-{run_id}.md`，subagent → `agent-{id}.md`），流式 assistant 块原地 rewrite；每次 dialogue 写入后刷新 **YAML frontmatter 摘要**（turns / tokens / cost）。`-f vortex` 仅写 `{run}/events.vortex`；事后可用 `traj materialize` 补全 md。run 结束写 `reconcile.json` 对账，不一致时再 materialize。

**文件名**（由 `storage_session_id` 决定）：

| 角色 | 典型文件名 |
|------|------------|
| 主 agent（`traj capture`） | `run-{run_id}.md` |
| Subagent | `agent-{claude_agent_id}.md` |
| 扁平 session | `{session_id}.md` |

Markdown 路径解析见 [轨迹存储模型 §7.3](trajectory_storage.zh.md)。

---

## 1. 设计意图

| 目标 | 说明 |
|------|------|
| **可读** | 正文是纯 Markdown，元数据藏在 HTML 注释 |
| **可解析** | 每块带 JSON 元数据，可 compact 回 Vortex |
| **可 diff** | 适合 git 跟踪 Agent 行为变化 |
| **有损物化** | 相对 Vortex 省略内部流量与 lifecycle；见 [存储模型 §1](trajectory_storage.zh.md) |

---

## 2. 块模型（TLV）

每块由两部分组成：

```
[类型/长度 元数据]  ← <!-- persisting:block:{speaker} {json} -->
[值 正文]           ← 裸 Markdown 消息体
```

`{speaker}`：`user` / `assistant` / `tool` 等。`{json}` 含 `kind`、`seq`、`turn`、`call_id`、`model`、token 统计、spawn 关联等。流式采集过程中 assistant 块可带 `"draft": true`，finalize 后被同 `call_id` 的最终块覆盖。

### 2.2 Live upsert（`-f md`）

Proxy 路径下 Markdown 按 **`call_id` + `role`** upsert，而非仅末尾 append：

| Event 变体 | md 行为 |
|------------|---------|
| `Event::Request` | user 块 append（或同 call 替换） |
| `Event::ResponseDraft` | assistant 块 upsert，`draft: true` |
| `Event::ResponseComplete` | assistant 块 upsert，移除 draft |

Import 路径仍使用批量 **append**。详见 [轨迹存储 §5.3](trajectory_storage.zh.md)。

### 2.4 块头 JSON 字段规范

块注释内 JSON 对象字段（`serde` flatten 与 `type` / `length` 并列；线上序列化时 `length` 在 `type` 之后）。

| 字段 | 必填 | 说明 |
|------|:----:|------|
| `type` | ✅ | 固定 `"markdown"`（存储单元类型，不是 `CaptureRecord.kind`） |
| `length` | ✅ | 正文 **UTF-8 字节长度**，含 [§2.5](#25-subagent-脚注) 脚注行；编码时由 `encode_block_with_header` 写入 |
| `v` | 推荐 | 块头 schema 版本，当前为 **`1`**；解析器应忽略未知版本或按迁移表处理 |
| `format_block` | 可选 | 字符串别名 **`"1.0"`**，与文档级 `format: persisting:1.0` 对应；工具可只实现其一 |
| `kind` | ✅ | `CaptureRecord.kind`（如 `llm.request`、`llm.response.stream`） |
| `role` | ✅ | 与 `{speaker}` 一致（`user` / `assistant` / `tool` / `note` …） |
| `source` | 推荐 | 记录来源（如 `persisting-proxy`、`markdown`） |
| `seq` | 推荐 | Vortex 行序号（物化块通常与 canonical 一致） |
| `turn` | 可选 | **仅展示用**：`seq / 2 + 1` 启发式；**不得**作为 Story / frontmatter `turns` 的权威轮次（权威值见 `story_user_turn_count` 或 `TurnMachine`） |
| `call_id` | live 推荐 | upsert 键之一（与 `role` 配对）；import-only append 可为空 |
| `draft` | 可选 | `true` 表示流式 assistant 草稿；finalize 后同键 upsert 覆盖并移除 |
| `session_id` / `agent_id` | 可选 | 会话与代理标识 |
| `timestamp` / `trace_id` / `parent_uuid` | 可选 | 追溯字段 |
| `model` / `path` | 可选 | `llm.request` |
| `status` / `prompt_tokens` / `completion_tokens` / `total_tokens` / `ttft_ms` | 可选 | `llm.response` / `llm.response.stream` |
| `subagent_id` / `parent_agent_id` / `subagent_trajectory` / `subagent_trajectories` / `spawn_hints` / `spawn_links` / `refs_subagent_ids` | 可选 | Spawn / 子代理关联（优先放在块头；脚注仅人读） |

实现常量：`persisting_capture::markdown_trajectory::BLOCK_FORMAT_VERSION`（`v`）、`BLOCK_FORMAT_BLOCK`（`"1.0"`）。

### 2.5 Subagent 脚注

写入正文末尾（计入 `length`），**不参与**消息 roundtrip：

```html
<!-- persisting:subagent-self agent-abc.md -->
<!-- persisting:subagent-refs other-a.md other-b.md -->
```

- **编码**：`append_subagent_refs_footer` 在可见文本之后追加。
- **解析 / compact**：`strip_subagent_footer_from_body` 在 `block_to_capture_record` 与可见文本提取前剔除**整行**脚注（行首 trim 后匹配 `<!-- persisting:subagent-` … `-->`）。
- **权威元数据**：`subagent_id`、`subagent_trajectories` 等仍以块头 JSON 与 Vortex 为准；脚注供人读与 grep。

### 2.6 大文件 upsert（演进，格式不变）

当前 live 路径对单文件执行 **读全量 + `rewrite_block_range`**（按 `call_id` + `role`），长会话下 IO 与 git diff 会抖动。

**计划（不改变块线格式）**：

1. 追加 **append-only journal**（例如同目录 `.{session}.md.journal` 仅 append 新块或 upsert 指令）。
2. 周期性 **compact** 合并 journal + 基线 md，产出规范 TLV 文件。
3. `tail -f` 可读 journal 或合并后的 md。

块头 `v` / `format_block` 即为该演进预留兼容位。

### 2.7 多模态对话正文（Phase 0）

当请求或响应含图像时，**Markdown 块正文与 Vortex 摘要字段**使用占位符，不内联 base64 像素（避免 git diff 与 frontmatter 膨胀）。

| 类型 | 占位符格式 | 说明 |
|------|------------|------|
| 用户 URL 图 | `[image: url:{url}]` | OpenAI `image_url`、Anthropic `image.source.url`、Responses `input_image` |
| 用户 base64 图 | `[image: base64:{size} {media_type} hash={blake3}]` | data URL 或 Anthropic `source.base64`；hash 为内容指纹前 12 hex |
| 助手生成图 | `[image_generated: {id}, {format}, {size}, ~{bytes}]` | Responses `image_generation_call`；可附 `prompt: {revised_prompt}` |

示例（user 块正文）：

```markdown
What's in this screenshot?

[image: url:https://example.com/screen.png]
```

示例（assistant 块正文，Codex 出图）：

```markdown
[image_generated: ig_062bc…, png, 1024x1536, ~1MB]
prompt: A gray tabby cat hugging an otter…
```

**计数**：含占位符的用户 turn 计入 frontmatter `turns` 与 `traj stats`（纯图无文字也算 1 轮）。

**与 `capture_level` 的关系**：

| 级别 | dialogue 占位符 | 原始图像 bytes |
|------|-----------------|----------------|
| `dialogue` | ✅ | ❌ |
| `full` | ✅ | 在 `payload.body` 中保留完整 JSON（含 base64），仍不进 Markdown 正文 |

**物化限制**：`traj materialize` 当前**不会**把占位符展开为 `<img>`；完整像素仅在 `full` 级 Vortex `payload.body` 或后续 sidecar Phase 1 中可恢复。详见 [Capture 架构 §6.4](capture_design.zh.md#64-可见对话提取含多模态)。

### 2.8 完整示例

```markdown
---
format: persisting:1.0
block: "<!-- persisting:block:{speaker} {json} -->\n\nmessage body\n\n"
session: run-20260526-103000
agent: my-project
model: deepseek-chat
started: 2026-05-26T10:00:00Z
duration: 42s
turns: 8
total_tokens: 18234
estimated_cost_usd: 0.073
subagents:
  - agent-abc123
client:
  command: claude
---

<!-- persisting:block:user {"type":"markdown","v":1,"kind":"llm.request","source":"persisting-proxy","seq":0,"turn":1,"role":"user","session_id":"demo-run-001","model":"deepseek-chat","path":"/v1/chat/completions","length":6} -->

你好

<!-- persisting:block:assistant {"type":"markdown","v":1,"kind":"llm.response","source":"persisting-proxy","seq":0,"turn":1,"role":"assistant","session_id":"demo-run-001","status":200,"prompt_tokens":12,"completion_tokens":18,"total_tokens":30,"length":39} -->

你好！有什么可以帮你的？
```

YAML frontmatter 固定 `format: persisting:1.0`，并含会话级 rollup（见 [轨迹存储 §8.1](trajectory_storage.zh.md)）。

**Golden 示例**（由 `encode_block_with_header` 生成，CI 校验）：

- 测试 fixture：[`crates/persisting-capture/tests/fixtures/tlv/demo-run-001.md`](../../crates/persisting-capture/tests/fixtures/tlv/demo-run-001.md)
- 文档示例副本：[`examples/trajectory-tlv/demo-agent/demo-run-001/0001.md`](../../examples/trajectory-tlv/demo-agent/demo-run-001/0001.md)

重新生成：`WRITE_TLV_GOLDEN=1 cargo test -p persisting-capture --test tlv_golden demo_run_001_matches_golden_fixture`

---

## 3. 使用场景

| 场景 | 行为 |
|------|------|
| Capture `-f md` | **仅** CaptureEngine live upsert 到 run bucket / `agent-*.md`（可 `tail -f`） |
| Capture `-f vortex` | 仅 Vortex（`events.vortex`） |
| `traj materialize` | 全量扫描 Vortex，**重写**指定 session 的 Markdown（修复/补全；traj capture 需 `--root-session-id`） |
| `traj add` + markdown 存储 | append Vortex 后自动 materialize |
| `traj add` + 输入为 `.md` | 解析 TLV → compact/append 到 Vortex |
| `traj replay` | 有 Vortex 时读 Vortex；纯 md session 从块还原 |

---

## 4. 与 Vortex 的关系

| 维度 | Markdown（物化） | Vortex（canonical） |
|------|------------------|---------------------|
| 角色 | 人读视图 | 全量 event log |
| 单位 | 块 | 行 |
| 写入 | live upsert / materialize / compact 导入 | 唯一 append 目标 |
| 数据完整性 | 有损（过滤内部事件） | 无损 |
| 典型操作 | 打开、git diff | replay、stats、Search |

双向转换由 `persisting-capture::trajectory_convert` 与引擎 `trajectory::convert` 实现。Subagent 块头字段与 [§2.5](#25-subagent-脚注) 脚注分工见上。详见 [轨迹存储模型](trajectory_storage.zh.md)。

---

## 5. Compact 回 Vortex

从 Markdown 导入 Vortex 时：

- 每块 → 一条 `CaptureRecord`（`llm.request` / `llm.response` 等）
- 块头字段额外写入 `payload._tlv`，保留 TLV 元数据
- 完整 HTTP body 若未出现在块中，则由正文近似重建

因此 **Markdown → Vortex 可压缩存储更多结构化字段**，但无法恢复 materialize 时已丢弃的内部事件。

---

## 6. 相关文档

- [轨迹存储模型](trajectory_storage.zh.md)
- [Capture 架构设计](capture_design.zh.md)
- [Traj 命令](cli_trajectory_command.zh.md) · [Capture 子命令](cli_capture_command.zh.md)
- [`persisting trajectory` 命令](cli_trajectory_command.zh.md)
