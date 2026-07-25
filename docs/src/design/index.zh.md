# 架构与内部实现

这些文档解释 Persisting 的实现方式。使用某项能力时，请优先阅读[选择能力](../guide/index.md)
中的已支持工作流。

## 按子系统阅读

| 子系统 | 先读 | 再读 |
|---|---|---|
| Queue | [队列持久化](architecture.zh.md) | [自定义后端指南](../guide/custom-backends.md) |
| Capture 与轨迹 | [Capture 管线](capture.md) | [轨迹存储](trajectory.md) → [Markdown 格式](trajectory-format.md) |
| Compute | [Compute 控制面](compute.md) | [Compute 指南](../guide/compute.md) |
| Tensor Memory（实验性） | [TTAS 模型](tensor-address-space.md) | [分层存储](distributed-tiered-storage.md) → [BlockStore](block-store.md) |
| CLI 边界 | [CLI 整体架构](cli.md) | **参考**中的命令文档 |

## 成熟度与范围

| 区域 | 状态 | 说明 |
|---|---|---|
| Capture、Queue、Search、Compute | 已实现 | 各自有独立的产品路径和存储模型 |
| TTAS / 分层张量内存 | 实验性 | 已有 host/SSD 工作；GPU 与跨节点数据路径仍在规划 |
| 竞品与系统比较 | 参考 | 为后续设计提供输入，不构成产品承诺 |

## 设计原则

1. 保持用户编程模型小而且与能力匹配。
2. 当子系统需要列式存储时，使用 Lance 作为耐久基线。
3. 分离控制面、数据移动和用户执行。
4. 在 TTAS 端到端数据路径完成前，把它视为实验性内部底座。
5. 明确失败和恢复语义，不暗示 exactly-once 保证。
