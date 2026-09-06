# 记录数据、查询视图与派生版本

pChronicle 把“发生了什么”与“如何查看它”分开。

| 层次 | 职责 | 示例 |
| --- | --- | --- |
| 记录事实 | 持久的写入时事实 | lifecycle、模型、工具、输出和终态 event |
| 查询视图 | 规范化查询模型 | `runs`、`steps`、`tool_calls`、`trajectories` |
| 人读视图 | 便于诊断的输出 | AgenticMD |
| 交换格式 | 互操作边界 | ATIF、ACTF、OpenAI Messages、Storyline JSON |
| 派生版本 | 记录来源的转换数据 | 清洗、脱敏或增广 Run |

Canonical event 是 append-oriented 的事实。Projection 可以为会话或查询重新组织这些事实，
但不能静默成为第二个事实源。可重建视图需要记录输入 Snapshot 和 transform version。

Storyline 是会话导向的 projection。它的三表 Lance 布局为完整文档重建优化，不是时序数据库，
也不替代 canonical event 路径。

AgenticMD 是非权威的人读 projection。Markdown 视图缺失或过期不会改变 canonical event 结果。

对于 Gateway 配套的单 trace 观测，Warehouse 可以在 Catalog 已经定位 source 后重新打开最新的
canonical event manifest。这样正在进行中的 trace 可以保持最新，而不要求每次追加事件都发布
物化 Storyline projection。

存储 API 把派生版本称为 Revision。它指向 parent 和生成它的 transform。清洗、脱敏和增广因此创建新的历史分支，
而不是无痕改写历史。

所有权见[Run 存储设计](../design/trajectory-storage.md)，精确交换契约见
[运行数据格式](../reference/formats/index.md)。
