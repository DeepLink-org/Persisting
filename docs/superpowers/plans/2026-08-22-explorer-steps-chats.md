# Explorer Steps / Chats Implementation Plan

> **For agentic workers:** Implement in this session. TDD for grouping; UI wires the same functions.

**Goal:** Trace 默认 Chats，可切 Steps；分组只在前端。

**Architecture:** 纯函数 `group_chats` 把 `TurnSummary[]` 收成 `TraceCard`。`TrajectoryView` 按 `view` 渲染。URL `view=chats|steps`，旧值 `tree` 视为 chats。

**Tech Stack:** Dioxus 0.7, Rust unit tests in `pchronicle-web`

## Global Constraints

- 不改 Storyline / ATIF / Lance
- 不伪造 user turn
- 详情仍用 `turn.id`

---

## Task 1: 分组函数

- [x] `pchronicle-web/src/chat_view.rs` 先写失败测试，再实现 `normalize_trace_view` / `group_chats`
- [x] `cargo test --manifest-path pchronicle-web/Cargo.toml --bin pchronicle-web -- chat_view`

## Task 2: Trace UI

- [x] 工具栏 Chats / Steps；默认 chats
- [x] Chats 卡片 + Steps 平铺 turn 行
- [x] 嵌入 Copilot 引用保持 Steps
