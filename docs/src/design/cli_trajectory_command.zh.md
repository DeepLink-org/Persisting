# `persisting trajectory` / `traj` — 轨迹 CLI

**`traj`** 是轨迹的统一入口：包含 **Ingress**（`traj capture` / `traj proxy` / `traj import`）与 **Egress**（`stats` / `replay` / `materialize` / …）。在 Run/Story 坐标上读写 **Vortex 事实源**（`CaptureRecord`）与 **TLV Markdown 物化视图**，语义与 [Capture 架构](capture_design.zh.md) §4 一致。

短名：**`traj`**（`trajectory` 全名）。Capture 采集子命令见 [Capture 子命令设计](cli_capture_command.zh.md)。

---

## 1. 核心坐标（与 Capture 对齐）

| Capture 概念 | CLI 参数 | 目录 |
|--------------|----------|------|
| **Storage** | `<STORAGE>` | 采集根目录 |
| **Agent** | `--agent-id` | `{storage}/{agent_id}/` |
| **Run** | `--root-session-id` | `{storage}/{agent_id}/{run_id}/` |
| **Story** | `--session-id` | Run 下主 session / subagent stem；Vortex 为 run 级 `events.vortex`（按 `session_id` 过滤），Markdown 为 `{key}.md` |

Subagent：`{run}/subagents/{session_id}/`。路径可传 session 目录，CLI 自动推断上述字段。

---

## 2. 命令一览

### Ingress（采集）

| 命令 | 说明 |
|------|------|
| **`traj capture`** | 一次性：进程内代理 + 子命令 |
| **`traj proxy`** | 前台长期代理 |
| **`traj proxy start\|stop\|list\|status`** | 守护进程生命周期与观测 |
| **`traj import`** | IDE / 网关日志事后导入 |
| **`traj replay-dead-letter`** | 重放 `.capture/dead_letter.jsonl` |

### Egress（读写 store）

| 命令 | 说明 |
|------|------|
| **add** | TOML/JSONL/Markdown → `CaptureRecord` → **写入单一层** |
| **truncate** | 保留 Vortex 前 N 行（按 `seq`） |
| **stats** | 双层摘要；`--detail` 逐轮树 |
| **replay** | 分页输出事件 JSON |
| **extract** | 拷贝 Story/Run 目录树 |
| **materialize** | Vortex → Markdown 全量物化 |

```text
persisting traj capture     [OPTIONS] -- <CMD>
persisting traj proxy         -o DIR -c FILE [OPTIONS]
persisting traj proxy start   -o DIR -c FILE [OPTIONS]
persisting traj add           <STORAGE> [OPTIONS]
persisting traj truncate  <STORAGE> [OPTIONS]
persisting traj stats     <STORAGE> [OPTIONS]
persisting traj replay    <STORAGE> [OPTIONS]
persisting traj extract   <STORAGE> <OUT_DIR> [OPTIONS]
persisting traj materialize <STORAGE> [OPTIONS]
```

实现：`persisting-capture` 的 `storage::{story_coords, path_layout, egress, convert}`；`persisting-engine` 负责 Vortex/Markdown store 与 RPC。

---

## 3. 存储层（`--storage-format`）

两种物理层：**Vortex**（canonical）、**Markdown**（视图）。**写命令每次只动一层**；读命令可 `auto` 探测。

| 命令 | `auto` 写入 | 显式 `--storage-format` |
|------|-------------|-------------------------|
| add | 无层 → Vortex；仅 md → Markdown；仅 Vortex → Vortex；两层都有 → Vortex | `vortex` / `markdown` 强制单层 |
| truncate | — | 仅 Vortex（按 `seq` 截断） |
| replay | 有 Vortex 读 Vortex；仅 md 读 md；都有 → Vortex | 可强制 `markdown` |
| stats | 两层都有 → **同时摘要** | 可强制单层 |
| materialize | — | Vortex → Markdown 全量导出 |

两层对齐：先 `add`（Vortex）或采集，再 `materialize`；**不会**在 `add`/`truncate` 时自动双写。

---

## 4. 示例

```bash
# 追加 JSONL 到 Vortex
persisting traj add ./store --agent-id a --session-id s --format jsonl --input batch.jsonl --storage-format vortex

# 截断 Vortex（Markdown 需单独 materialize）
persisting traj truncate ./store --agent-id a --session-id s --keep-rows 100

# 双层统计（只读）
persisting traj stats ./store --agent-id a --root-session-id run-1 --session-id run-1

# 从 Vortex 补人读视图
persisting traj materialize ./store --agent-id a --session-id s --root-session-id run-1
```

---

## 5. 与 Capture 实时路径

| 路径 | 入口 |
|------|------|
| 实时采集 | `persisting traj capture` / `traj proxy`（Ingress + live md） |
| 离线运维 | `traj stats` / `replay` / `materialize` / … |
