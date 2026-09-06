# pVisor 核心概念

pVisor 围绕 **Agent Run** 构建，而不是围绕进程、Container 或虚拟机。比较 Provider 或
解释 Run Bundle 前，先理解这些概念。

:::note 什么时候阅读这里
完成第一次 Run 后，如果你需要知道实际隔离了什么、哪些修改仍在 stage 中，或为什么不同执行
Provider 的保证不同，就从这里开始。
:::

请按以下顺序阅读：

1. [什么是 AgentVisor？](agentvisor.md) 解释产品类别，以及 Agent 与运行时之间的边界。
2. [Run、Attempt 与 Effect](run-model.md) 定义可以跨进程和 Provider 保留的稳定对象。
3. [Capability 与 Evidence](capabilities-and-evidence.md) 解释请求如何变成实际机制，以及如何
   写入 Run Bundle。

品类文章不绑定具体实现；Run 和 capability 文章定义 pVisor 稳定的用户模型；平台机制与
当前缺口属于 [pVisor Design](../design/index.md)。

读完本节后，你应该能查看 Run Bundle，并区分请求的 capability 与实际生效的 capability。
准备做 Provider 或 policy 决策时，继续阅读[实用指南](../guides/index.md)。
