# Codex / Claude Code 会话接入 pChronicle

日期：2026-08-23
状态：已实现；命令示例已同步到 canonical CLI
范围：`persisting-pchronicle` 文档格式探测与解码、本地 JSONL 文件源、`pchronicle import` / `query` / `ls` / `analysis` / `status`
非目标：Codex/Claude 回写、专用 CLI flag、默认 Warehouse、监听 `~/.codex`、把 subagent 合并进父 session、RFC 级往返契约

## 1. 决策

Codex 与 Claude Code 的本地 JSONL 会话，登记为 **decode-only** 的 `DocumentFormat`。解码器产出 `StorylineDocument`，再走现有 `runs` / `steps` / `tool_calls` 投影。

两条入口共用这一套解码器，地位相同：

```bash
pchronicle query ~/.codex/sessions --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
pchronicle import --from ~/.codex/sessions --to ./codex-ds
```

```bash
pchronicle query ~/.claude/projects --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
pchronicle import --from ~/.claude/projects --to ./claude-ds
```

`ls` / `status` / `analysis` 对同一路径同样有效。不把其中一条标成主路径或降级路径。

不新增 `--from-codex`。路径就是数据源。不把 `~/.codex/sessions` 设成默认 Warehouse。对 `~/.codex` / `~/.claude` 只读。

锁定方案 **A**（解码型一等格式，无 encode）：

- `DocumentFormat::Codex`（`codex`）
- `DocumentFormat::ClaudeCode`（`claude-code`）
- CLI `--input-format codex` / `--input-format claude-code`
- `encode_json_storylines` 与 `export --output-format codex|claude-code` **拒绝**（fail closed）
- 未映射事件进 `unknown_fields`，不承诺以后能 roundtrip

## 2. 架构

```text
~/.codex/sessions/**/rollout-*.jsonl ──┐
~/.claude/projects/**/*.jsonl         ─┤
显式 --format 的 stdin / 单文件        ─┼── detect ── decode ── StorylineDocument
                                       │
                                       ├── catalog / FileTrajectoryDataSource ── query / ls / analysis / status
                                       └── import preserve | import --output-format storyline
```

一个 JSONL 文件 = 一条 Storyline / 一条 run。Claude subagent 文件各自一条；能读到父 `sessionId` 时填 `parent`。

模块：

| 位置 | 职责 |
|---|---|
| `formats/codex.rs`、`formats/claude_code.rs` | JSONL 解析与探测线索 |
| `convert/codex.rs`、`convert/claude_code.rs` | → `StorylineDocument` |
| `formats/detect.rs` | 内容优先；`rollout-*.jsonl` 仅作 Codex 辅助 |
| `document.rs` | decode 接入；encode fail closed |
| `store/files/`、`LocalQueryManifest` | 允许这两种格式；扩展名 `jsonl`（及 `json` / `ndjson` 若出现） |
| `store/catalog/` | 每文件探测；无需新 catalog 类型 |
| CLI `ExchangeFormat` | import 可 auto 或显式；export 拒绝这两种 |

读取按 ATIF 同款 **JSONL 流式** 路径，不把整个 rollout 读进内存。`~/.codex/sessions` 量级约为数百文件、数十万 `response_item`。

## 3. 探测

内容优先，避免误伤 ATIF / OpenAI Messages / Storyline / ACTF。

**Codex**（取首条非空 JSONL 行）：

- 对象含 `timestamp`、`type`、`payload`
- 且 `type` ∈ `{session_meta, response_item, event_msg}`
- 路径辅助：文件名匹配 `rollout-*.jsonl`

**Claude Code**：

- 对象 `type` ∈ `{user, assistant, system}`
- 且存在 camelCase `sessionId` 或 `uuid`
- **不**把带 `session_id` + `step_id` 的 OpenAI corpus 认成 Claude

**冲突规则：**

- OpenAI Messages 继续要求 `session_id` + `step_id`（及 messages / response）
- ATIF 继续要求 `schema_version` 以 `ATIF` 开头，或 `steps`+`agent`
- 目录 auto-import / catalog `--errors report`：认不出的 JSON skip + warning
- 单文件、stdin、显式 `--format`：探测失败则硬失败

探测有两条现有路径，本规格都要接通，不发明第三种：

- **Catalog**（CLI `query` / `ls` / `analysis` / `status` 对目录或 Dataset）：**按文件** 探测。同一棵树里 ATIF 与 Codex 可以并存（与现有多格式 Dataset 相同）。
- **`LocalQueryManifest::detect`**（`ChronicleQueryEngine::open` 直开一个目录）：以稳定相对路径下的 **第一个** JSON/JSONL 定格式，再只收该格式的扩展名。`~/.codex/sessions` 与 `~/.claude/projects` 是纯目录，适用此规则。混放多种轨迹格式的目录直开不在保证范围内（与现有 ATIF/OpenAI 目录直开相同）。

CLI 对 Dataset 的 SQL 用 `dataset.runs` / `dataset.steps` / `dataset.tool_calls`；库 `ChronicleQueryEngine::open` 用无前缀的 `runs` / `steps` / `tool_calls`。这是现有差别，不为本功能改名。

## 4. 字段映射

权威对话流只用会话正文事件。UI / 遥测事件不另开 turn。未映射字段进入 `unknown_fields`，source 为 `codex` 或 `claude-code`。

### 4.1 会话头

| Storyline | Codex | Claude Code |
|---|---|---|
| `origin.format` | `codex` | `claude-code` |
| `session` | `session_meta.payload.id`；否则文件名中的 UUID | 记录上的 `sessionId`；否则文件名（去 `.jsonl`） |
| `agent.id` | `codex`；`source.subagent` 为真时 `codex-subagent` | `agentId` 若有，否则 `claude-code` |
| `agent.model` | 最近一条 `turn_context.payload.model` | 助手消息 `message.model` |
| `agent.ver` | `cli_version` | `version` |
| `task.env.name` | `cwd` | `cwd` |
| `started_at` / `finished_at` | 首末事件 `timestamp` | 同上 |
| `parent.psid` | `parent_id` | 子 agent 文件能读到父 `sessionId` 则填 |

### 4.2 Codex 正文（`response_item.payload`）

按文件顺序扫描。同一 `internal_chat_message_metadata_passthrough.turn_id` 的 assistant 文本、reasoning、tool call **收成一条** `src=agent` turn。

| `payload.type` | 映射 |
|---|---|
| `message` role=`user` | `src=user`，`msg` = `input_text` 文本拼接；图片进 unknown |
| `message` role=`assistant` / `agent_message` | `src=agent`，`msg` = `output_text` |
| `message` role=`developer` | 并入 `prompt.system`，不开 turn |
| `reasoning` | 当前或下一条 agent turn 的 `reason`；仅有 `encrypted_content` 则不放正文，指针进 unknown |
| `function_call` | `tool_calls[]`：`tcid=call_id`，`fn=name`，`args` 解析 JSON 字符串，失败则保留字符串 |
| `custom_tool_call` | 同上，`fn=name`，`args={ "input": input }`，`kind=custom` |
| `tool_search_call` | `fn=tool_search`，args 原样 |
| `function_call_output` / `custom_tool_call_output` / `tool_search_output` | 按 `call_id` 填对应 `result`；对不上则挂当前 agent turn 的 `observation` |

`event_msg` **不**做第二套对话（否则会与 `response_item` 双计）：

- `token_count` → 当前 agent turn `metrics`
- `task_complete` → `task.result`
- 其余（`task_started`、`thread_settings_applied`、`mcp_tool_call_end` 等）进 unknown

`turn_context` 只更新后续 agent turn 的 `model`。`world_state` / `compacted` / `inter_agent_communication_metadata` 整段 unknown。

### 4.3 Claude Code 正文

文件顺序即 turn 顺序。忽略 `parentUuid` 树，避免分叉打乱线性 Storyline。`isSidechain` 写入 turn `extra`。

| `type` | 映射 |
|---|---|
| `user`，content 为文本 | `src=user` |
| `user`，content 含 `tool_result` | 按 `tool_use_id` 填上一轮 tool `result`；纯 result 的 user 行不另开 user turn |
| `assistant` 文本块 | `src=agent`，`msg` 拼接 `text` |
| `assistant` `thinking` | `reason` |
| `assistant` `tool_use` | `tcid=id`，`fn=name`，`args=input` |
| `system` / `progress` / `compact_boundary` / `summary` | unknown，不生成 turn |

### 4.4 空文件与残缺

- 只有 `session_meta`、零 turn：仍解码为一条 run（`StorylineDocument::validate` 允许空 `turns`）
- 显式 `--format` 且首行对不上：硬失败
- auto 目录认不出：skip + warning

## 5. 错误处理

与现有 import / catalog 边界一致。

| 情况 | 行为 |
|---|---|
| 目录 + format auto，文件不像已知轨迹 | skip + stderr warning |
| 单文件 / stdin / 显式 `--format`，探测失败或首行对不上 | `InvalidRequest` |
| 已判定为 `codex` / `claude-code`，某行 JSON 损坏或缺必填 | 硬失败，带相对路径和行号 |
| `session` 在同一次 import 输入之间碰撞 | 整次 import 失败 |
| import 目标已存在 | 拒绝覆盖 |
| `export --format codex\|claude-code`（含 `--strict`） | `Unsupported`，不写半成品 |
| 空 jsonl / 仅 `session_meta` | 一条 0-turn Storyline |
| 超大文件 | JSONL 流式读取；`--max-input-bytes` / query 文件上限仍生效 |
| 未知 `payload.type` / Claude `type` | 进 unknown，**不**中断该文件 |

Query 直开 `~/.codex/sessions` 时：strict 发现策略下，无法解码的文件使整次 query 失败；`--errors report` 时跳过并警告。不修改 vendor 目录。

## 6. Import 行为

默认 `--output-format preserve`：Dataset 内保留原始相对路径（如 `2026/08/03/rollout-….jsonl`）。随后对该 Dataset 的 query / analysis 走同一解码器。

`--output-format storyline`：压成一份 Storyline Lance，行为与现有 squash 路径相同。

`document_id` / `session_id` 全局唯一规则不变。Codex rollout UUID 与 Claude 文件名 UUID 作为默认 session。

## 7. 测试

不把 `~/.codex/sessions` 或 `~/.claude/projects` 真会话提交进仓库。夹具为手写最小 JSONL：

- `crates/persisting-pchronicle/tests/fixtures/codex/session.jsonl`：`session_meta` + user/assistant `message` + `function_call`/`output` + `custom_tool_call`/`output` + `reasoning` + `token_count` + 一条应进 unknown 的 `world_state`
- `crates/persisting-pchronicle/tests/fixtures/claude-code/session.jsonl`：user 文本、assistant 文本 + `tool_use`、下一条 user `tool_result`、一条 `compact_boundary`
- 对照：坏行、空 session、ATIF / OpenAI Messages 首行（不得误判）

矩阵：

1. **探测**：`rollout-*.jsonl` 路径与首行指纹；不误伤 ATIF / OpenAI / Storyline / ACTF
2. **转换**：夹具 → Storyline；断言 session、src、msg、tool `tcid`/`fn`/`result`、`reason`、token metrics；`world_state` / `compact_boundary` 在 unknown
3. **encode**：`encode_json_storylines(Codex | ClaudeCode)` 返回 unsupported
4. **直查**：`ChronicleQueryEngine::open` 对夹具目录执行 `SELECT COUNT(*) FROM runs`，文件数为 run 数；`_file_` 为相对路径。CLI 对同一目录走 catalog，SQL 为 `FROM dataset.runs`。
5. **CLI import preserve**：目录递归；Dataset 内相对路径保留；query `dataset.runs` 计数等于文件数
6. **CLI import `--output-format storyline`**：squash 后仍能 query
7. **CLI export** 这两种 format 失败
8. **显式 `--format` + 坏行** 失败且信息含行号
9. **auto 目录混入无关 JSON** skip，其余仍导入

手工验收（实现者本机有会话时）：

```bash
pchronicle query ~/.codex/sessions --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
pchronicle query ~/.codex/sessions \
  --sql 'SELECT session_id, COUNT(*) AS tool_calls FROM dataset.tool_calls GROUP BY session_id LIMIT 5'
pchronicle ls ~/.codex/sessions
pchronicle analysis overview ~/.codex/sessions

pchronicle import --from ~/.codex/sessions --to ./codex-ds
pchronicle query ./codex-ds --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'

pchronicle export --from ./codex-ds --to /tmp/x.json --output-format codex
# 必须失败
```

Claude 将 `~/.codex/sessions` 换成 `~/.claude/projects`。本机 `~/.claude/projects` 为空时，CLI 夹具覆盖直查与 import，不把空目录当失败。

## 8. 文档

更新：

- `docs/src/pchronicle/guides/exchange.md` 与 `.zh.md`
- `docs/src/pchronicle/reference/formats/index.md` 与 `.zh.md`
- `docs/src/pchronicle/guides/discover-and-query.md`（若列出可直查格式）
- CLI `--help`

写明 `codex` / `claude-code` 是 decode-only 会话日志，不是可往返 interchange。不单开 RFC。

## 9. 非目标

- 专用 flag（`--from-codex`）或默认指向 `~/.codex/sessions`
- 监听 / 增量同步 vendor 目录
- 把 subagent 文件合并进父 session
- 重建 Codex Responses JSONL 或 Claude transcript
- 把 `event_msg.user_message` 再投影一遍
- 解密 `encrypted_content`
- 保证未来 vendor 新增事件类型都进入领域字段（未知 type → unknown，文件继续成功）
