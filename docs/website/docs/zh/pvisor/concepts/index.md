# pVisor 核心概念

pVisor 围绕 **Agent Run** 构建，而不是围绕进程、Container 或虚拟机。比较 Provider 或
解释 Run Bundle 前，先理解这些概念。

请按以下顺序阅读：

1. [什么是 AgentVisor？](agentvisor.md) 解释产品类别，以及 Agent 与运行时之间的边界。
2. [Run、Attempt 与 Effect](run-model.md) 定义可以跨进程和 Provider 保留的稳定对象。
3. [Capability 与 Evidence](capabilities-and-evidence.md) 解释请求如何变成实际机制，以及如何
   写入 Run Bundle。

品类文章不绑定具体实现；Run 和 capability 文章定义 pVisor 稳定的用户模型；平台机制与
当前缺口属于 [pVisor Design](../design/index.md)。
