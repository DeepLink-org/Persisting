# 事实、Projection 与 Revision

pChronicle 把“发生了什么”与“如何查看它”分开。

| 层次 | 职责 | 示例 |
| --- | --- | --- |
| Canonical facts | 持久的写入时事实 | lifecycle、模型、工具、Artifact 和终态 event |
| Logical projection | 规范化查询模型 | `runs`、`steps`、`tool_calls`、`trajectories` |
| Human projection | 人读诊断视图 | AgenticMD |
| Exchange representation | 互操作边界 | ATIF、ACTF、OpenAI Messages、Storyline JSON |
| Revision | 带 lineage 的派生数据 | 清洗、脱敏或增广轨迹 |

Canonical event 是 append-oriented 的事实。Projection 可以为会话或查询重新组织这些事实，
但不能静默成为第二个事实源。可重建视图需要记录输入 Snapshot 和 transform version。

Storyline 是会话导向的 projection。它的三表 Lance 布局为完整文档重建优化，不是时序数据库，
也不替代 canonical event 路径。

AgenticMD 是非权威的人读 projection。Markdown 视图缺失或过期不会改变 canonical event 结果。

Revision 指向 parent 和生成它的 transform。清洗、脱敏和增广因此创建新 lineage，
而不是无痕改写历史。

所有权见[轨迹存储](../design/trajectory-storage.md)，精确交换契约见
[轨迹格式](../reference/formats/index.md)。
