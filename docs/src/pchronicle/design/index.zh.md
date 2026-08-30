# pChronicle Design

这些页面解释运行数据源如何变成持久、可查询的历史。下文使用的存储和 API 术语见
[术语指南](../reference/terminology.zh.md)。

| 区域 | 文档 |
| --- | --- |
| 产品边界与运维保证 | [Architecture](architecture.md) |
| Path 身份、Snapshot 同步与惰性 Source resolve | [Snapshot](catalog.md) |
| Canonical event 与 projection ownership | [运行存储](trajectory-storage.md) |
| Storyline 三表 projection 与内容层 | [Storyline Lance](storyline-lance.md) |

当前命令与格式见 [pChronicle Reference](../reference/index.md)，跨产品 ownership 见
[System Design](../../system-design/index.md)。
