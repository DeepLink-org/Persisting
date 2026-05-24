# 轨迹存储模型

Agent 轨迹在 Persisting 中同时服务两类读者：**机器**（回放、统计、检索）与**人**（阅读、diff、审查）。采用**两层结构**：Lance 为 canonical raw event log，TLV Markdown 为物化的人读视图。

---

## 1. 两层结构

| 层 | 格式 | 角色 | 特点 |
|----|------|------|------|
| **Raw event log** | Lance v1 | **Canonical** | 全量事件，无损；列存；所有 append 的唯一落点 |
| **Dialogue view** | TLV Markdown (`{session}.md`) | **物化视图** | 人读对话块；由 Lance materialize 派生，可过滤内部流量 |

```
Capture / trajectory add
      │
      ▼
 Lance raw event log     ← canonical（seq + payload_json + 索引列）
      │
      │ materialize（有损，按需）
      ▼
 TLV Markdown            ← 人读视图

 Markdown 文件 / 手工编辑
      │
      │ compact（重建 CaptureRecord → Lance）
      ▼
 Lance raw event log     ← 列存；payload 可含 _tlv 元数据
```

### 转换语义

| 方向 | API | 模式 |
|------|-----|------|
| **Lance → Markdown** | `stream_engine_lines_to_markdown` | **流式**：每批新 events append 块（capture `-f md`） |
| **Lance → Markdown** | `materialize_lance_to_markdown` | **全量**：扫描 Lance 重写 md（CLI `materialize`） |
| **Markdown → Lance** | `compact_markdown_to_lance` | 重建 |

**Materialize 时跳过的事件**（不进入 Markdown）：

- `count_tokens` 等内部探测请求
- 空 turn（无可见 user/assistant 正文）
- `session.started` / `session.ended` / `session.state` 等 lifecycle
- 主 session 上重复的 flash/haiku companion 探测（subagent 目录内保留）

**Invariant**：materialize 之后 `Lance row_count ≥ Markdown block_count`。

---

## 2. 设计目标

| 目标 | 说明 |
|------|------|
| **单一写入点** | 运行时只 append Lance；不再 capture 时并行写 Markdown |
| **双视图** | Lance 适合 replay / Search；Markdown 适合 git diff |
| **可互转** | materialize / compact 保持两层同步 |
| **与捕获解耦** | 捕获层产出 `CaptureRecord`；存储层决定物化时机 |

---

## 3. 分层架构

```
数据源（代理流量 / IDE 日志 / 历史导出）
        │
        ▼ 归一化为 CaptureRecord
    捕获与对话层
        │
        ▼
    轨迹引擎（append → Lance；materialize / compact）
        │
   ┌────┴────┐
   │         │
 Lance    TLV Markdown
（canonical） （materialized）
```

---

## 4. Lance v1（Raw event log）

路径：`{storage}/{agent_id}/{session_id}/`（嵌套 subagent 见 [代理文档](llm_capture_proxy.zh.md)）。

每 session 一个 Lance dataset，每行一条事件：

| 列 | 说明 |
|----|------|
| `seq` | 会话内单调序号 |
| `timestamp`, `kind`, `source` | 事件元数据 |
| `agent_id`, `session_id` | 路由 / 过滤 |
| `call_id`, `trace_id`, `parent_call_id` | 调用链 |
| `model` | 从 payload 反规范化 |
| `payload_json` | 完整 `CaptureRecord` JSON（**canonical 载荷**） |

适合：`replay`、`stats`、Search import、未来 FTS / 多模态 blob 引用。

---

## 5. TLV Markdown（Dialogue view）

- 文件名：`{session_id}.md`（兼容读取 legacy `0001.md`、`trajectory.tlv.md`）
- 每块 = `<!-- persisting:block:{speaker} {json} -->` + 裸正文
- **不由 capture 直接 append**；由 materialize 整文件重写或首次生成
- 详见 [轨迹 Markdown 格式](trajectory_tlv_format.zh.md)

---

## 6. 存储策略（`--storage-format` / `-f`）

| 策略 | Append 行为 | Markdown |
|------|-------------|----------|
| **lance** (`-f bin`) | 仅写 Lance | 不生成 |
| **markdown** (`-f md`) | 写 Lance + **流式 append** md | 随批次增量生成 |
| **both** | 同 markdown（legacy 别名） | 物化生成 |
| **auto** | 空 session → Lance；已有数据按探测结果 | 不自动物化 |

**读取优先级**：`replay` / `stats` 默认以 Lance 为准（若存在）；纯 Markdown session 从块序列还原。

### Capture 写入路径（`-f md` / `-f bin`）

1. 代理捕获 → `CaptureRecord`
2. 后台 worker **批量** append Lance（默认 32 条/批）
3. **`-f md`**：每批 Lance 写入后 **流式 append** 对应对话块到 `{session}.md`（`tail -f` 可实时阅读）
4. **`-f bin`**：仅 Lance

`trajectory add --storage-format markdown`：Lance append 后对**同一批** lines 流式 append Markdown。

`trajectory materialize`：全量扫描 Lance **重写** Markdown（修复/补全，非 capture 热路径）。

---

## 7. 目录示例

```
{store}/
  {agent_id}/
    {session_id}/
      data.lance/          ← Lance v1 dataset（canonical）
      {session_id}.md      ← materialize 产出（可选）
      subagents/
        {sub_session_id}/
          ...
```

---

## 8. 逻辑事件

不论物理形态，一条轨迹事件包含：**序号、来源、类型、时间、会话/调用标识、载荷**（及未来可选 blob 引用）。

常见 `kind`：`llm.request`、`llm.response`、`llm.response.stream`、`llm.spawn_link`、`session.started`、`session.ended`、…

---

## 9. 相关文档

- [轨迹 Markdown 格式](trajectory_tlv_format.zh.md)
- [内嵌 LLM 代理与 Capture](llm_capture_proxy.zh.md)
- [`persisting capture`](cli_capture_command.zh.md)
- [`persisting trajectory`](cli_trajectory_command.zh.md)
