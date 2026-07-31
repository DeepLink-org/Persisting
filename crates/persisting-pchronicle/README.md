# pChronicle

**Canonical Run History Store** — Persisting 组件，负责运行事实、终态提交与可重建视图。

## 格式架构：storyline 为枢纽

`storyline` 以 **ATIF-v1.7** 为基准（Trajectory / Step 折叠语义），作为唯一互操作枢纽；外围格式只与它互转：

```text
events ──┐
agenticmd ┼──► storyline ──► events / agenticmd / openai_msg / atif
openai_msg┤
atif ─────┘
```

| 名称 | 角色 | 含义 | 典型产物 |
|---|---|---|---|
| `storyline` | **枢纽** | ATIF-aligned Trajectory/Step + 短名 wire / 性能顶栏 | `storyline.json` |
| `events` | 外围 + SoT（**仅 Lance**） | 原始交换日志；JSON/JSONL 不是支持的 wire | `events.lance` |
| `agenticmd` | 外围 | TLV Markdown 对话视图 | `{session}.md` |
| `openai_msg` | 外围 | OpenAI messages 步表 | `session_steps.json` |
| `atif` | 外围 | Harbor ATIF interchange | 三表 JSONL |

API：`into_storyline` / `from_storyline` / `convert`。

CLI：

```bash
persisting traj convert <INPUT> -o <DEST> -f storyline|atif|openai_msg|agenticmd|events [--from …]
```

示例：`traj convert storyline.json -o out.md -f agenticmd`；`events` 读写会话目录 / `events.lance`（非 JSONL）。

## 与 ATIF

| ATIF | Storyline |
|---|---|
| `session_id` | `session` |
| `trajectory_id` | `run` |
| `steps[]` | `turns[]`（`id` ↔ `step_id`，`src`/`msg` ↔ `source`/`message`） |
| step 折叠字段 | 同义保留；`latency_ms`/`ttft_ms`/`duration_ms` 可从 metrics/extra 提升到顶栏 |

## ATIF 三表（`atif` 外围）

| 表 | 主键 |
|---|---|
| `sessions` | `session_id` |
| `steps` | (`session_id`, `step_id`) |
| `tool_calls` | (`session_id`, `tool_call_id`) |

`atif_trajectory` view 仍可用于三表扁平行查询；进出系统时优先经 storyline。

## 规范

- [RFC-0001: Storyline Format](../../docs/src/rfcs/0001-storyline-format.md)
- [RFC-0002: Events Format](../../docs/src/rfcs/0002-events-format.md)

## events：仅 Lance

`events` **不是** JSON/JSONL 格式。采集与存储只写 `events.lance`。

- 字符串 API（`into_storyline` / `from_storyline` / `convert`）对 `ChronicleFormat::Events` 会报错。
- 从 Lance 读出行后，用内存 API：`events_to_storyline` / `storyline_to_events`。
- 调试导出可用 `export_events_jsonl`（非正式格式）；日常请用 `traj` 等工具从 Lance 抽取。
