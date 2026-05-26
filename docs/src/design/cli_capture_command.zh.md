# `persisting capture` 设计说明

Capture 将 **LLM API 流量** 与 **IDE 本地会话** 归一化为统一轨迹，写入 [轨迹存储](trajectory_storage.zh.md) 所描述的 **两层结构**（Lance canonical + TLV Markdown 物化）。

---

## 1. 两条路径

```
                    ┌─────────────────────┐
  LLM SDK / Agent ──│ 内嵌代理 (主路径)   │──► Lance event log（按 storage_session_id 分 dataset）
                    └──────────┬──────────┘
                               │ -f md：CaptureEngine live upsert md
                               ▼
                    TLV Markdown（主 run-*.md + sibling agent-*.md）

  IDE JSONL / 历史 OTLP ──► import (补充路径) ──► 合并进同一 store
```

| 路径 | 何时用 |
|------|--------|
| **代理捕获** | 开发/运行 Agent 时，让流量经过 Persisting 代理 |
| **事后导入** | 补录 Claude/Cursor 本地日志，或合并旧版网关导出 |

主路径**不依赖**外部 agentgateway。详见 [Capture 架构设计](capture_design.zh.md)。

---

## 2. 命令族

| 族 | 职责 |
|----|------|
| **start / stop** | 后台守护进程生命周期 |
| **serve** | 前台代理，便于调试 |
| **run** | 代理 + 执行一条子命令（典型：`claude`、训练脚本） |
| **list / status** | 查看会话、用量、成本估算 |
| **import** | 从 IDE 或网关日志批量导入 |
| **replay-dead-letter** | 重放 `.capture/dead_letter.jsonl` 中失败的采集事件 |

公共选项概念：

| 选项 | 含义 |
|------|------|
| 输出目录 | 轨迹 store 根路径 |
| 代理配置 | 模型路由与 upstream YAML |
| **格式 `-f`** | 见下节 |

### 格式 `-f md|bin`

| 值 | 运行时写入 | Markdown |
|----|------------|----------|
| **`md`（默认）** | Lance（批量）+ Proxy **CaptureEngine live upsert** Markdown（主 → `run-{run_id}.md`，subagent → `agent-{id}.md`） |
| **`bin` / `lance`** | Lance only |

Capture 不在结束时自动全量 materialize；`-f md` 时 Proxy 内 **CaptureEngine** 随 LLM 事件 upsert Markdown（流式 assistant 块原地 rewrite），可用 `tail -f` 跟踪。run 结束自动写 `.capture/reconcile.json`；**仅当对账不一致时**再执行 `trajectory materialize`（见 [轨迹存储 §8.2](trajectory_storage.zh.md)）。

---

## 3. `capture run` 工作流

一次 `run` 代表 **一次完整的捕获会话**：

1. 在进程内启动代理（不依赖已存在的守护进程）
2. 分配本次 run 的 session 标识，写入 `session.started` 到 Lance
3. 为子进程设置代理与环境（base URL、session id 等）
4. 子进程 LLM 请求被捕获 → Worker 批量 append Lance；**`-f md`** 时 Proxy **CaptureEngine** 对 md 做 live upsert（主/子文件隔离，见 [Capture 架构 §8](capture_design.zh.md)）
5. 子进程退出 → 写入 `session.ended` → 代理关闭

若同一目录已有存活守护进程，`run` 会拒绝，避免 store 冲突。

---

## 4. 守护进程模式

适合长期本地开发：一次 `start`，多个终端的 Agent 共用同一代理与 store。

- 全程 append **Lance**（Worker 批量 flush）
- `-f md` 时 Proxy **CaptureEngine live upsert** Markdown（长期 daemon 可 `tail -f` 阅读）

`stop` / `list` / `status` 可省略输出目录（自动解析最近一次 start 的位置）。

---

## 5. 数据源（import）

| 来源 | 说明 |
|------|------|
| Claude Code | 项目目录下的 JSONL 会话文件 |
| Cursor | agent-transcripts 下的 JSONL |
| 网关 OTLP | 可选；历史 agentgateway 导出的 JSONL |

import 支持按项目、时间范围过滤，可选合并 subagent 文件，dry-run 仅统计不写盘。写入经 trajectory 引擎 append（Lance）。

---

## 6. 轨迹事件（概念）

归一化后的事件带：**序号、来源、类型、时间、会话标识、载荷**。

| 来源示例 | 典型类型 |
|----------|----------|
| 代理 | `llm.request`、`llm.response`、`llm.response.stream` |
| Lifecycle | `session.started`、`session.ended` |
| IDE | 用户消息、助手回复、工具调用 |
| 关联 | `llm.spawn_link`、subagent 引用 |

Lifecycle 与内部探测保留在 **Lance**；materialize 到 Markdown 时通常**不**出现。

---

## 7. 与其它工具

- **RTK**：压缩 shell 输出，不捕获 LLM；与 capture 无依赖关系。
- **Trajectory CLI**：对已有 store 做 replay / stats / materialize；capture 负责生产 Lance 数据。

---

## 8. 相关文档

- [Capture 架构设计](capture_design.zh.md)
- [轨迹存储模型](trajectory_storage.zh.md)
- [轨迹 Markdown 格式](trajectory_tlv_format.zh.md)
- 分步示例：[`examples/capture-walkthrough/README.md`](../../examples/capture-walkthrough/README.md)
