# pVisor 核心概念

pVisor 围绕 **Agent Run** 构建，而不是围绕进程、Container 或虚拟机。比较 Provider 或
解释 Run Bundle 前，先理解这些概念。

| 问题 | 概念文章 |
| --- | --- |
| 什么基础设施品类负责虚拟化 Agent 执行？ | [什么是 AgentVisor？](agentvisor.md) |
| 哪些状态跨越进程和 Provider 变化？ | [Run、Attempt 与 Effect](run-model.md) |
| 如何请求权限并报告实际 enforcement？ | [Capability 与 Evidence](capabilities-and-evidence.md) |

品类文章不绑定具体实现；Run 和 capability 文章定义 pVisor 稳定的用户模型；平台机制与
当前缺口属于 [pVisor Design](../design/index.md)。
