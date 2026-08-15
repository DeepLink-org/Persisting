# 从本地到集群

可移植单位是逻辑 Run，而不是一台正在运行的虚拟机。

![AgentVisor 执行连续体](../assets/diagrams/agentvisor/execution-continuum.svg)

从本地 placement 迁移到集群时，以下内容必须保持稳定：

- Run identity 与父子 lineage；
- Delegated authority 及其 generation；
- Semantic checkpoint 与 effect frontier；
- Artifact identity 与持久 evidence；
- 终态结果 ownership。

进程、Kernel、root filesystem、Node、Scheduler 与 execution provider 可以变化。只有
能够满足 Run 所请求 capability 维度的 Provider 才能通过 admission。不支持的保证必须
显式失败，不能在迁移后静默弱化。

在个人设备上，主要体验是 staged workspace 与可审查 Effect；在集群中，同一模型增加
placement、tenant isolation、lease、attestation、恢复与 reconciliation，而不重新定义 Run。

稳定 identity 模型见 [Run、Attempt 与 Effect](../pvisor/concepts/run-model.md)。Provider
admission 属于 [pVisor 隔离设计](../pvisor/design/isolation.md)，多 Run lease 与 reconciliation
属于 [pPilot](../pvisor/design/orchestration.md)。
