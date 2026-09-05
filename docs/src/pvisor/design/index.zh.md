# pVisor Design

这些页面解释 pVisor 如何实现一个 Agent 虚拟执行环境。

| 区域 | 文档 |
| --- | --- |
| Provider 组合与安全属性 | [隔离架构](isolation.md) |
| VM 透明截获与规划中的 host 截获 | [OverlayNet](overlaynet.md) |
| 模型路由、capture 与 event emission | [Gateway](gateway.md) |
| 产品命令模型与生命周期语义 | [CLI 设计](cli.zh.md) |

跨产品的 Run 与历史 ownership 见 [System Design](../../system-design/index.md)。面向用户的
行为以 [pVisor 使用指南](../guides/index.md)为准，而不是以设计文档中的 roadmap 为准。
