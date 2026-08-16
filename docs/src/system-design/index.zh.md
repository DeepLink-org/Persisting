# System Design

Persisting 有两个主要产品域：

- [pVisor](../pvisor/index.md) 虚拟化并治理 Agent 执行；
- [pChronicle](../pchronicle/index.md) 保存并查询持久 Run 历史。

pPilot 把 pVisor 从一个 Run 扩展到多个 Run。Gateway、OverlayFS 与 OverlayNet 是 pVisor
运行时机制。两个产品通过稳定 Run identity、capture event、Artifact、终态结果与 lineage
连接。

![Persisting product domains and integration](../assets/diagrams/persisting/system-products.svg)

## 跨产品契约

```text
Agent goal
  → RunSpec
  → pVisor / pPilot 拥有执行与 Attempt state
  → EventIngest + Artifact + RunResult
  → pChronicle 拥有持久历史与派生视图
```

| 关注点 | Owner |
| --- | --- |
| 单个 Run 的执行边界 | pVisor |
| 多个 Run 的 planning 与恢复 | pPilot |
| 模型、网络与文件系统 runtime driver | pVisor |
| Canonical event 与 Dataset 历史 | pChronicle |
| 查询、交换与 revision lineage | pChronicle |

## 按问题继续阅读

- [完整架构与目标模型](architecture.md)
- [从本地到集群的连续性](local-to-fleet.md)
- [安全与 Evidence 模型](security-evidence.md)
- [pVisor 实现边界](../pvisor/design/index.md)
- [pChronicle 实现边界](../pchronicle/design/index.md)

交付状态以产品 Design 页面与[项目工程笔记](../project/engineering.md)为准。目标架构不能
作为功能已经实现的证据。
