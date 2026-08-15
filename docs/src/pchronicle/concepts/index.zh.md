# pChronicle 核心概念

pChronicle 保留历史的物理来源，并把写入时事实与读取时 projection 分开。

| 问题 | 概念文章 |
| --- | --- |
| 历史如何寻址，并在一次操作中固定版本？ | [Dataset、Source 与 Snapshot](dataset-and-source.md) |
| 事实、规范化视图、交换格式和 lineage 分别由哪一层拥有？ | [事实、Projection 与 Revision](facts-and-projections.md) |

这些文章定义稳定心智模型。查询步骤属于[使用指南](../guides/index.md)，精确 schema 和格式
属于[参考](../reference/index.md)，存储机制属于[设计](../design/index.md)。
