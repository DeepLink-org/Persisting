# Explorer Trace：Steps / Chats 两种前端视图

## Status

Implemented. Approved in conversation on 2026-08-22: two frontend-only layouts;
default open is **Chats**; names are **Steps** and **Chats**.

## Context

Harbor ATIF 一个 step 只有一个 `source`（`system` / `user` / `agent`）。
Storyline turn 对齐这个模型。聊天习惯则把「用户一句 + 随后的助手回复」看成一轮交互。

后端不改 wire、不合成 turn、不发明 user 行。两种读法都在 `pchronicle-web`
对已加载的 `TurnSummary[]` 做分组。

现有 Trace 按 `call_id` 收成 span，那是工具跨度，不是对话轮。Chats 另做一层，
不和 span 混用同一套 key。

URL 已有未使用的 `view`（默认曾是 `tree`）。本功能占用它。

## Goals

1. Trace 工具栏可切换 **Chats** / **Steps**。打开 Run 默认为 Chats。
2. 分组、计数、卡片结构只在前端完成；点开仍用原始 `turn.id` 拉详情。
3. Analysis、source 过滤、`turn=` 深链继续针对原始 turn。
4. ACTF 目前几乎全是 `src=agent` 时，Chats 不强行画用户气泡。

## Non-goals

- 改 Storyline / ATIF / ACTF 映射或 Lance 投影。
- 用 `/prompt.user` 伪造 `src=user` turn。
- 引入 exchange / chat id 或后端聚合 API。
- 改 Analysis 图表的聚合口径。

## Decision

### 名称与默认

| 开关 | URL | 含义 |
|---|---|---|
| **Chats** | `view=chats`（默认） | 交互轮：一个 user 开一轮，随后连续 agent 是这轮回复 |
| **Steps** | `view=steps` | ATIF 步：一条 turn 一行，一个 `source` |

未知或旧值 `tree` 视为 `chats`。刷新保持 `view`。

### Chats 分组（数组顺序，稳定）

对当前列表（已 overlay source / 文本过滤）从左到右扫：

1. `user` 开启一轮，吃掉后面连续的 `agent`。
2. 下一个 `user` 或 `system` 结束上一轮。
3. 单独的 `system`（含 compaction）自成一轮，不并入相邻 chat。
4. 没有前置 `user` 的 `agent` 各自自成一轮（只有助手侧）。
5. 连续多个 `user`：各开一轮；没有 agent 的 user 也是一轮。

主界面仍是原来的 span 表 + occupancy 时间轴。Chats 把一轮交互收成一行
（bar 覆盖该轮 `event_seqs`）；展开后仍是原来的 turn 行。

### Steps

一条 turn 一行，同样走 span 表和时间轴，不再用 `call_id` 合并。点开证据与现在相同。

### 过滤与 URL

- source / `turn_q` 只在前端过滤。Chats 先分组再按「行里是否含该 source / 文本」
  决定显隐，匹配行保留全部成员。Steps 仍按单条 turn 滤。不再为过滤重拉 API。
- 轴标题是 Sequence / occupancy；bar 按 `user` / `agent` / `system` 上色。
- Chat 行 Overview 是 user preview，没有 user 则 `No user turn`。
- 助手侧文案统一 Copilot。
- `turn=` 仍是 turn id。Chats 下若该 id 在某轮内，展开那一轮并选中该 turn。
- 不新增 query 参数。

## Files

- `pchronicle-web/src/components.rs` — 分组函数 + Chats / Steps 列表
- `pchronicle-web/src/workspace.rs` — 工具栏开关；`view` 默认 `chats`
- 现有 `assets/*.css` 加 Chats 卡片样式，不新开 CSS 管线

## Test

- 分组单测：user→agent→agent、前导 agent、中段 system、连续 user。
- 不要求后端 / e2e。
