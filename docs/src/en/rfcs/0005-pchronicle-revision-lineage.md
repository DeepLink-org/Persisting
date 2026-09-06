# RFC-0005: pChronicle 派生 Revision Lineage

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Date** | 2026-08-09 |
| **Component** | `persisting-pchronicle` |
| **Related** | [RFC-0002 Events](0002-events-format.md) · [RFC-0003 Ownership](0003-pchronicle-ownership.md) |

## 摘要

clean、redact、augment 和格式化数据集是 canonical events 的派生产物。它们不得覆写或
去重事实流，也不得把 catalog 语义塞入 Storyline `extra_json`。每个 Run 使用独立的
`revisions.lance` 记录 lineage；canonical `events.lance` 继续维持 at-least-once、
append-only 契约。

## 数据模型

每个 revision 以 `revision_id` 为 upsert key，包含：

- `parent_revision_ids`：零个或多个父 revision；
- `kind`：`clean`、`redact`、`augment`、`export` 或扩展 kind；
- `canonical_snapshot`：输入 event manifest revision或 Storyline snapshot；
- `recipe`：可重放的程序、版本、参数和输入摘要 JSON；
- `status`：`building`、`ready` 或 `failed`；
- `created_at` 与 `output_refs`。

`revisions.lance` 是派生 catalog，不参与 canonical event 的提交、replay 或审计裁定。
写入相同 `revision_id` 更新状态和产物引用；不同 revision 永不隐式合并。

## 一致性与回收

产物必须先持久化，再把 revision 切到 `ready`。失败记录可保留用于诊断。对象维护将
`CURRENT` 以及保留的 `ready` revision 视为 GC roots；删除 revision 本身必须是显式维护操作。

## 验收

- 同一 revision 从 `building` 更新为 `ready` 只留下一个 catalog row。
- lineage 写入不改变 canonical event 行数或 manifest。
- catalog 可被 Web UI 和 DataFusion 消费，并能定位 recipe 与产物。
