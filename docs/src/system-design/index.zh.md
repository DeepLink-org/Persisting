# System Design

Persisting 是横跨模型状态——参数与 KV Cache——以及 Agent 历史的持久化基础设施。本节聚焦
当前两个公开产品域：

- [pVisor](../pvisor/index.md) 虚拟化并治理 Agent 执行；
- [pChronicle](../pchronicle/index.md) 把持久轨迹 Source 组织为可查询 Dataset。

Gateway、OverlayFS 与 OverlayNet 是 pVisor 运行时机制。存在稳定 Run identity 时，它会连接
两个产品域，但二者也各有独立入口。

![Persisting 产品域与集成关系](../assets/diagrams/persisting/system-products.svg)

## 跨产品契约

```text
Configured pVisor capture
  Gateway trajectory events ─┐
  pVisor lifecycle records ──┴─> canonical event Source ─┐
Pinned external Sources                                  │
  ATIF / ACTF / OpenAI Messages / Storyline ─────────────┴─> Catalog Snapshot
                                                               └─> normalized Dataset views
```

Attempt finalization 会写入私有、带版本的 Run Bundle，并保留 staged Effect，供之后执行
review/apply/drop；这一过程不需要 pChronicle。配置后的 capture 会发送 Gateway 轨迹 event
与 pVisor lifecycle record，包括这些 record 携带的 Evidence。完整 Bundle 及其中的 Artifact、
lineage、Effect 与更完整的 Evidence 清单仍留在本地，除非另行搬运。

外部文件与 Storyline Source 会被直接固定版本并规范化，无需经过 pVisor，也不会先变成
canonical event。每条路径保留与 Source 对应的保证；ingestion 不会补充 Source 未提供的
Evidence。

| 关注点 | Owner |
| --- | --- |
| 单个 Run 的执行边界 | pVisor |
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
