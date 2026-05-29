# `persisting trajectory` / `traj` — Capture Egress CLI

Trajectory 命令是 **Persisting Capture 离线 Egress 面**：在 Run/Story 坐标上读写 **Lance 事实源**（`CaptureRecord`）与 **TLV Markdown 物化视图**，语义与 [Capture 架构](capture_design.zh.md) §4 一致。

短名：**`traj`**。

---

## 1. 核心坐标（与 Capture 对齐）

| Capture 概念 | CLI 参数 | 目录 |
|--------------|----------|------|
| **Storage** | `<STORAGE>` | 采集根目录 |
| **Agent** | `--agent-id` | `{storage}/{agent_id}/` |
| **Run** | `--root-session-id` | `{storage}/{agent_id}/{run_id}/` |
| **Story** | `--session-id` | Run 下主 session / subagent stem；Lance 在 `.lance/{key}`，Markdown 为 `{key}.md` |

Subagent：`{run}/subagents/{session_id}/`。路径可传 session 目录，CLI 自动推断上述字段。

---

## 2. 命令一览（Egress）

| 命令 | Capture 语义 | 说明 |
|------|--------------|------|
| **add** | 反向 Ingress | TOML/JSONL/Markdown → `CaptureRecord` → **写入单一层**（`lance` 或 `markdown`） |
| **truncate** | Egress 写回 | 保留 Lance 前 N 行（按 `seq`）；不自动改 md |
| **stats** | 读模型 + 双层摘要 | `auto` 同时报 Lance 行数与 Markdown 块数；`--detail` 逐轮树 |
| **replay** | Egress 重放 | 分页输出事件 JSON；两层并存时默认 **Lance** |
| **extract** | Egress 导出 | 拷贝 Story/Run 目录树到 `<OUT_DIR>`；`--include-subagents` 含子树 |
| **materialize** | Lance → Markdown | 全量有损物化（维护/对账补 md） |

```text
persisting traj add       <STORAGE> [OPTIONS]
persisting traj truncate  <STORAGE> [OPTIONS]
persisting traj stats     <STORAGE> [OPTIONS]
persisting traj replay    <STORAGE> [OPTIONS]
persisting traj extract   <STORAGE> <OUT_DIR> [OPTIONS]
persisting traj materialize <STORAGE> [OPTIONS]
```

实现：`persisting-capture` 的 `storage::{story_coords, path_layout, egress, convert}`；`persisting-engine` 负责 Lance/Markdown store 与 RPC。

---

## 3. 存储层（`--storage-format`）

两种物理层：**Lance**（canonical）、**Markdown**（视图）。**写命令每次只动一层**；读命令可 `auto` 探测。

| 命令 | `auto` 写入 | 显式 `--storage-format` |
|------|-------------|-------------------------|
| add | 无层 → Lance；仅 md → Markdown；仅 Lance → Lance；两层都有 → Lance | `lance` / `markdown` 强制单层 |
| truncate | — | 仅 Lance（按 `seq` 截断） |
| replay | 有 Lance 读 Lance；仅 md 读 md；都有 → Lance | 可强制 `markdown` |
| stats | 两层都有 → **同时摘要** | 可强制单层 |
| materialize | — | Lance → Markdown 全量导出 |

两层对齐：先 `add`（Lance）或采集，再 `materialize`；**不会**在 `add`/`truncate` 时自动双写。

---

## 4. 示例

```bash
# 追加 JSONL 到 Lance
persisting traj add ./store --agent-id a --session-id s --format jsonl --input batch.jsonl --storage-format lance

# 截断 Lance（Markdown 需单独 materialize）
persisting traj truncate ./store --agent-id a --session-id s --keep-rows 100

# 双层统计（只读）
persisting traj stats ./store --agent-id a --root-session-id run-1 --session-id run-1

# 从 Lance 补人读视图
persisting traj materialize ./store --agent-id a --session-id s --root-session-id run-1
```

---

## 5. 与 Capture 实时路径

| 路径 | 入口 |
|------|------|
| 实时采集 | `persisting capture run`（Ingress + live md） |
| 离线运维 | **`traj`**（本页） |
