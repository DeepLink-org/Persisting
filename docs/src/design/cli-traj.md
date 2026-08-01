# `persisting trajectory` / `traj` — 轨迹 CLI

**`traj`** 是 pChronicle 轨迹数据入口：负责 import 与 stats/replay/materialize 等数据操作。实时执行和 Gateway 生命周期由并列的 `pvisor run` / `persisting traj proxy` 命令负责。

短名：**`traj`**（`trajectory` 全名）。单 Run 实时采集由 [`pvisor run`](cli-pvisor.md) 负责。

---

## 1. 核心坐标（与 Capture 对齐）

| Capture 概念 | CLI 参数 | 目录 |
|--------------|----------|------|
| **Storage** | `<STORAGE>` | 采集根目录 |
| **Agent** | `--agent-id` | `{storage}/{agent_id}/` |
| **Run** | `--root-session-id` | `{storage}/{agent_id}/{run_id}/` |
| **Story** | `--session-id` | Run 下主 session / subagent stem；Lance 为 run 级 `events.lance`（按 `session_id` 过滤），Markdown 为 `{key}.md` |

Subagent：`{run}/subagents/{session_id}/`。路径可传 session 目录，CLI 自动推断上述字段。

---

## 2. 命令一览

### Ingress（采集）

| 命令 | 说明 |
|------|------|
| **`pvisor run`** | 一次性：进程内代理 + 子命令 |
| **`persisting traj proxy`** | 前台长期代理 |
| **`persisting traj proxy start\|stop\|list\|status`** | 守护进程生命周期与观测 |
| **`traj import`** | IDE / 网关日志事后导入 |
| **`traj replay-dead-letter`** | 重放 `.capture/dead_letter.jsonl` |

### Egress（读写 store）

| 命令 | 说明 |
|------|------|
| **add** | TOML/JSONL/Markdown → `CaptureRecord` → **写入单一层** |
| **truncate** | 保留 Lance 前 N 行（按 `seq`） |
| **stats** | 双层摘要；`--detail` 逐轮树；未指定 `--session-id` 时扫描 agent 下全部 run，并对 run bucket **展开** Lance 内 distinct `session_id` 后逐分区统计（见[轨迹存储](trajectory.md)的 run bucket 分区说明） |
| **replay** | 分页输出事件 JSON |
| **extract** | 拷贝 Story/Run 目录树 |
| **materialize** | Lance → Markdown 全量物化 |

```text
pvisor run     [OPTIONS] -- <CMD>
persisting traj proxy         -o DIR -c FILE [OPTIONS]
persisting traj proxy start   -o DIR -c FILE [OPTIONS]
persisting traj add           <STORAGE> [OPTIONS]
persisting traj truncate  <STORAGE> [OPTIONS]
persisting traj stats     <STORAGE> [OPTIONS]
persisting traj replay    <STORAGE> [OPTIONS]
persisting traj extract   <STORAGE> <OUT_DIR> [OPTIONS]
persisting traj materialize <STORAGE> [OPTIONS]
```

实现：`persisting-pchronicle` 拥有格式、路径、Lance/Markdown store 与领域服务；`persisting-engine` 只保留 CLI 动态 ABI 的 RPC 适配。

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

# 双层统计（只读；省略 --session-id 时扫描 agent 下所有 run / session 分区）
persisting traj stats ./store --agent-id a --detail

# 指定单个 header session UUID（Claude 对话分区）
persisting traj stats ./store --agent-id a --root-session-id run-1 --session-id 58867536-…

# 从 Lance 补人读视图
persisting traj materialize ./store --agent-id a --session-id s --root-session-id run-1
```

---

## 5. 与 Capture 实时路径

| 路径 | 入口 |
|------|------|
| 实时采集 | `pvisor run` / `persisting traj proxy`（Ingress + live md） |
| 离线运维 | `traj stats` / `replay` / `materialize` / … |
