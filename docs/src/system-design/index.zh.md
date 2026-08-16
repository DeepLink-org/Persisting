# System Design

Persisting 有两个主要产品域：

- [pVisor](../pvisor/index.md) 虚拟化并治理 Agent 执行；
- [pChronicle](../pchronicle/index.md) 把持久轨迹 Source 组织为可查询 Dataset。

pPilot 把 pVisor 从一个 Run 扩展到多个 Run。Gateway、OverlayFS 与 OverlayNet 是 pVisor
运行时机制。两个产品域通过稳定 Run identity、event、Artifact、终态事实、lineage 与
Evidence 集成，但各自都有独立入口。

![Persisting product domains and integration](../assets/diagrams/persisting/system-products.svg)

## 跨产品契约

```text
Agent goal -> pVisor / pPilot -> events + artifacts + terminal facts + Evidence
                                                   |
External Sources -> importer / adapter ------------+-> pChronicle Dataset
```

pVisor Run 可以在没有 pChronicle 的情况下，以可审查的 staged Effect 和私有、带版本的
Run Bundle 结束。标准持久交接会把它观察到的事实与 Evidence 发送到 pChronicle。外部
Source 无需经过 pVisor，也可通过受支持的 importer 或 adapter 进入同一 Dataset 模型。
每条路径保留与 Source 对应的保证；ingestion 不会补充 Source 未提供的 Evidence。

| 关注点 | Owner |
| --- | --- |
| 单个 Run 的执行边界 | pVisor |
| 多个 Run 的 planning 与恢复 | pPilot |
| 模型、网络与文件系统 runtime driver | pVisor |
| Canonical event、终态事实与 Dataset 历史 | pChronicle |
| 查询、交换与 revision lineage | pChronicle |

## 按问题继续阅读

- [完整架构与目标模型](architecture.md)
- [从本地到集群的连续性](local-to-fleet.md)
- [安全与 Evidence 模型](security-evidence.md)
- [pVisor 实现边界](../pvisor/design/index.md)
- [pChronicle 实现边界](../pchronicle/design/index.md)

交付状态以产品 Design 页面与[项目工程笔记](../project/engineering.md)为准。目标架构不能
作为功能已经实现的证据。
