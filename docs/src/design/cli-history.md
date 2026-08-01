# `persisting history` / `eval` / `gateway` — 命令参考

轨迹命令按职责拆为三个稳定入口：`history` 负责数据生命周期，`eval` 负责评测，
`gateway` 负责长期采集服务。pChronicle 只提供底层存储和查询能力。

单 Run 实时采集由 [`persisting execute`](cli-pvisor.md) 负责。

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
| **`persisting execute`** | 一次性：进程内代理 + 子命令 |
| **`persisting gateway serve`** | 前台长期代理 |
| **`persisting gateway start\|stop\|list\|status`** | 守护进程生命周期与观测 |
| **`persisting history import`** | IDE / 网关日志事后导入 |
| **`persisting history replay-dead-letter`** | 重放 `.capture/dead_letter.jsonl` |

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
persisting execute             [OPTIONS] -- <CMD>
persisting gateway serve       -o DIR -c FILE [OPTIONS]
persisting gateway start       -o DIR -c FILE [OPTIONS]
persisting history add         <STORAGE> [OPTIONS]
persisting history truncate    <STORAGE> [OPTIONS]
persisting history stats       <STORAGE> [OPTIONS]
persisting history replay      <STORAGE> [OPTIONS]
persisting history extract     <STORAGE> <OUT_DIR> [OPTIONS]
persisting history materialize <STORAGE> [OPTIONS]
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
persisting history add ./store --agent-id a --session-id s --format jsonl --input batch.jsonl --storage-format lance

# 截断 Lance（Markdown 需单独 materialize）
persisting history truncate ./store --agent-id a --session-id s --keep-rows 100

# 双层统计（只读；省略 --session-id 时扫描 agent 下所有 run / session 分区）
persisting history stats ./store --agent-id a --detail

# 指定单个 header session UUID（Claude 对话分区）
persisting history stats ./store --agent-id a --root-session-id run-1 --session-id 58867536-…

# 从 Lance 补人读视图
persisting history materialize ./store --agent-id a --session-id s --root-session-id run-1
```

---

## 5. 与 Capture 实时路径

| 路径 | 入口 |
|------|------|
| 实时采集 | `persisting execute` / `persisting gateway serve`（Ingress + live md） |
| 离线运维 | `persisting history stats` / `replay` / `materialize` / … |
