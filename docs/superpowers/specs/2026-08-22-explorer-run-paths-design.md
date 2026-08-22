# Explorer Run paths：squash 后不再用导入前目录

## Status

Approved as approach A on 2026-08-22.

## Context

`--output-format storyline` squash 之后，Catalog 只有一个 `_file_ = "."` 的 Source。
Run paths 仍把 `run_id`（常为原文件名 / job 名）当成根目录，拼出
`{dataset}/{run_id}/subagents/{session_id}`。这不是 Storyline 父子关系。

## Decision

当 `_file_ == "."`：

- 叶子用 `document_id`：`{dataset}/{document_id}`
- 仅当 `parent.psid` / `parent_session_id` 存在且不等于 `session_id` 时：
  `{dataset}/{parent}/subagents/{document_id}`
- **不用 `run_id` 做路径段**

`_file_ != "."`（preserve / 多文件 Catalog）保持现有 `{dataset}/{file}/…` 规则。
`RunSummary.root_session_id` 仍可回退到 `run_id`，只改展示路径。

## Non-goals

- 改 Catalog source 发现或 `_file_` 列
- 扁平化真实 parent/child
- 重编嵌入前端
