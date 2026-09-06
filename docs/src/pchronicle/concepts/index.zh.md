# pChronicle 核心概念

pChronicle 围绕 **Dataset** 构建，Dataset 就是 path。查询历史或集成存储前，先理解这些概念。

:::note 什么时候阅读这里
完成第一次查询后，如果你需要解释结果来自哪里、在调查期间固定 Snapshot，或区分 canonical
record 与派生视图，就从这里开始。
:::

请按以下顺序阅读：

1. [Dataset、Source 与 Snapshot](dataset-and-source.md) 解释 path 如何寻址、固定版本并保持
   一致读取。
2. [记录数据、查询视图与派生版本](facts-and-projections.md) 区分 canonical fact 与用于查看、
   交换的视图。

存储机制与当前实现属于 [pChronicle Design](../design/index.md)。接下来可以进入
[常见工作流](../guides/index.md) 或 [命令行参考](../reference/cli.md)。

读完本节后，你应该能用 Dataset、Source 与 Snapshot 描述一个结果，而不是把查询输出当成
无法追溯来源的文件。
