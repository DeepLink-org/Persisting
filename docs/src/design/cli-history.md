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
| **add** | TOML/JSONL/Markdown → `EventRecord` → canonical Lance |
| **stats** | canonical Lance 摘要，并附带 Markdown 调试视图信息；`--detail` 逐轮树；未指定 `--session-id` 时扫描 agent 下全部 run，并对 run bucket **展开** Lance 内 distinct `session_id` 后逐分区统计 |
| **replay** | 按 `--offset` / `--limit` 分页输出已有事件 |
| **extract** | 拷贝 Story/Run 目录树 |
| **materialize** | Lance → Markdown 全量物化 |

```text
persisting execute             [OPTIONS] -- <CMD>
persisting gateway serve       -o DIR -c FILE [OPTIONS]
persisting gateway start       -o DIR -c FILE [OPTIONS]
persisting history add         <STORAGE> [OPTIONS]
persisting history stats       <STORAGE> [OPTIONS]
persisting history replay      <STORAGE> [OPTIONS]
persisting history extract     <STORAGE> <OUT_DIR> [OPTIONS]
persisting history materialize <STORAGE> [OPTIONS]
```

实现：`persisting-pchronicle` 拥有格式、路径、canonical Lance store、AgenticMD 视图生成与领域服务；CLI 进程内直接调用 pChronicle。

---

## 3. Canonical 存储（`--storage-format`）

当前只有 **Lance** 是存储层。`auto` 与 `lance` 均选择 canonical Lance；Markdown 是
旁路调试视图，不参与自动探测或回退。

| 命令 | `auto` 写入 | 显式 `--storage-format` |
|------|-------------|-------------------------|
| add | Lance | `lance` |
| replay | Lance | `lance` |
| stats | Lance 行数为准；附带 Markdown 块数 | `lance` |
| materialize | — | Lance → Markdown 全量导出 |

AgenticMD 可删除后重新 materialize。修改 `.md` 不会改变 canonical events；如需导入
外部 Markdown，显式使用 `history add --format markdown`，解析结果仍写入 Lance。

---

## 4. 示例

```bash
# 追加 JSONL 到 Lance
persisting history add ./store --agent-id a --session-id s --format jsonl --input batch.jsonl --storage-format lance

# 双层统计（只读；省略 --session-id 时扫描 agent 下所有 run / session 分区）
persisting history stats ./store --agent-id a --detail

# 指定单个 header session UUID（Claude 对话分区）
persisting history stats ./store --agent-id a --root-session-id run-1 --session-id 58867536-…

# 从 Lance 补人读视图
persisting history materialize ./store --agent-id a --session-id s --root-session-id run-1

# 从 offset=0 分页输出已有事件
persisting history replay ./store --agent-id a --session-id s --offset 0 --limit 256
```

`replay` 保持 TOML 输出，`--offset` / `--limit` 表示一次分页。需要持续读取运行中已提交的
event micro-batch 时，使用 `persisting query follow`。

---

## 5. 与 Capture 实时路径

| 路径 | 入口 |
|------|------|
| 实时采集 | `persisting execute` / `persisting gateway serve`（Ingress + live md） |
| 实时事件查询 | `persisting query follow`（已提交 canonical event micro-batch） |
| 离线运维 | `persisting history stats` / `replay` / `materialize` / … |

实时查询统一使用 pPilot 所有的 `persisting query follow` 入口。

## 6. 本地 Web 分析

```bash
persisting chronicle serve ./store
```

该命令只监听 `127.0.0.1`，提供 Run/Event/Storyline 浏览、只读 SQL、HAR/OTLP 导出、
judgment、revision catalog 和显式 maintain。主视图使用 Storyline 阅读投影，Inspector 始终
回链 canonical event 的 `seq` / `event_id` / `call_id` 与原始 JSON；增长中的 Run 通过 SSE
提示并可自动跟随。它没有认证能力，因此拒绝绑定非 loopback 地址。
