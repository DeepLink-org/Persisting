# `persisting traj` Capture 子命令设计说明

Capture 采集能力已并入 **`persisting traj`**（`trajectory` 全名）：`traj capture`（一次性）、`traj proxy`（长期代理）、`traj import`（事后导入），与 `traj stats` / `replay` / `materialize` 等同属轨迹 CLI。

> **想先跑起来？** 见 [Capture 快速上手](../guide/capture_quickstart.md)。完整子命令列表见 [Traj 命令](cli_trajectory_command.zh.md)。

---

## 1. 两条路径

```
                    ┌─────────────────────┐
  LLM SDK / Agent ──│ 内嵌代理 (主路径)   │──► Vortex event log
                    └──────────┬──────────┘
                               │ -f md：CaptureEngine live upsert md
                               ▼
                    TLV Markdown（主 run-*.md + sibling agent-*.md）

  IDE JSONL / 历史 OTLP ──► traj import ──► 合并进同一 store
```

| 路径 | 何时用 |
|------|--------|
| **代理捕获** | `traj capture` / `traj proxy` 实时采集 |
| **事后导入** | `traj import` 补录 Claude Code JSONL 等 |

详见 [Capture 架构设计](capture_design.zh.md)。

---

## 2. 命令族

| 子命令 | 职责 |
|--------|------|
| **`traj capture`** | 进程内代理 + 执行子命令（`claude`、`codex`、脚本） |
| **`traj proxy`** | 前台长期代理 |
| **`traj proxy start`** | 后台守护进程 |
| **`traj proxy stop`** | 停止守护进程 |
| **`traj proxy list`** | 会话列表、用量、成本 |
| **`traj proxy status`** | 活跃连接与会话（admin API） |
| **`traj import`** | IDE / 网关日志事后导入 |
| **`traj replay-dead-letter`** | 重放 `.capture/dead_letter.jsonl` |

公共选项：`-o` store 根目录、`-c` 代理 TOML、**`-f md|vortex`**（见下）。

### 格式 `-f md|vortex`

| 值 | 运行时写入 | Markdown |
|----|------------|----------|
| **`md`（默认）** | **仅** CaptureEngine **live upsert** md（不写 Vortex） |
| **`vortex`** | 仅 Vortex 事件日志（`{run}/events.vortex`） |

`-f md` 时不自动全量 materialize；run 结束写 `.capture/reconcile.json`（md 与 replay 记录对账）；不一致时用 **`traj materialize`**（需已有 Vortex 层，见 [轨迹存储 §8.3](trajectory_storage.zh.md)）。

---

## 3. `traj capture` 工作流

1. 进程内启动代理（不依赖已存在守护进程）
2. 分配 run session，写入 `session.started`
3. 为子进程注入 `HTTP_PROXY`、`OPENAI_BASE_URL` 等
4. LLM 流量 → Vortex（`-f vortex`）或 CaptureEngine（`-f md`）live upsert Markdown
5. 子进程退出 → `session.ended` → 代理关闭

同一 `-o` 上若已有 `traj proxy` / `traj proxy start`，`traj capture` 会拒绝。

---

## 4. `traj proxy` / `traj proxy start`

| 命令 | 说明 |
|------|------|
| **`traj proxy`** | 前台；打印 proxy/admin 与 **如何使用**；`Ctrl+C` 停止 |
| **`traj proxy start`** | 后台（spawn `traj proxy` 子进程） |
| **`traj proxy stop` / `list` / `status`** | 守护进程运维 |

环境变量示例见 [快速上手 §3](../guide/capture_quickstart.md)。**`traj proxy` 不会**为其他终端自动注入环境；需手动 `export` 或使用 `traj capture`。

`list` / `status` / `stop` 可省略 `-o`（最近 `start` 或 `PERSISTING_CAPTURE_STORAGE`）。

---

## 5. `traj import`

| 来源 | 说明 |
|------|------|
| Claude Code | ✅ `~/.claude/projects/` JSONL |
| OpenAI Codex | ✅ 主路径为 `traj capture` / `traj proxy`，非 import |
| Cursor | ❌ 暂不支持 |
| 网关 OTLP | 可选 JSONL |

支持 `--dry-run`、`--since-days`、`--merge-subagents` 等。

---

## 6. 相关文档

- [Traj 命令（完整列表）](cli_trajectory_command.zh.md)
- [Capture 架构设计](capture_design.zh.md)
- [轨迹存储模型](trajectory_storage.zh.md)
- [Capture 快速上手](../guide/capture_quickstart.md)
- [分步示例](../../examples/capture-walkthrough/README.md)
