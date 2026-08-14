# 架构与内部实现

这些文档解释 Persisting 的实现方式。使用某项能力时，请优先阅读[选择能力](../guide/index.md)
中的已支持工作流。

## 按子系统阅读

| 子系统 | 先读 | 再读 |
|---|---|---|
| pVisor | [Agent 基础设施](agent-infrastructure.md) | [隔离后端](pvisor-isolation.md) → 独立 `pvisor` CLI → [Gateway 驱动](gateway.md) |
| pChronicle | [`pchronicle` 命令参考](cli-pchronicle.md) | [Dataset Catalog](dataset-catalog.md) → [轨迹存储](trajectory.md) → [Storyline 三表 Lance](storyline-lance.md) → [RFC-0003 Ownership](../rfcs/0003-pchronicle-ownership.md) |
| pPilot | [pPilot 控制面](ppilot.md) | 独立 `ppilot` CLI → Run 编排与 pChronicle SQL 分析 |
| Queue | [队列持久化](architecture.zh.md) | [自定义后端指南](../guide/custom-backends.md) |
| Gateway 采集驱动 | [Gateway 管线](gateway.md) | [Markdown 格式](trajectory-format.md) → [RFC-0001 Storyline](../rfcs/0001-storyline-format.md) / [RFC-0002 Events](../rfcs/0002-events-format.md) |
| CLI 边界 | [pVisor](cli-pvisor.md)、[pPilot](cli-ppilot.md)、[pChronicle](cli-pchronicle.md) | **参考**中的命令文档 |

## 成熟度与范围

| 区域 | 状态 | 说明 |
|---|---|---|
| pVisor、pPilot、pChronicle | 核心已实现 | 并列的 Agent 执行、编排与历史组件；pChronicle 公共产品面以命令参考为准 |
| Gateway、OverlayNet、OverlayFS | 已实现 | pVisor 运行时驱动；Gateway 提供 capture 语义 |
| pVisor 强制隔离 | 已实现 / 部分完成 | Linux 已有 FUSE + 最小 synthetic root + rootless namespace + Landlock；macOS 已有 Seatbelt 强制 staged 写入和 deny-all socket 约束，读取仍为 ambient；Docker 与 libkrun/KVM transport 已有，seccomp、资源控制、LiteBox VFS 与 Firecracker 仍在[隔离路线图](pvisor-isolation.md)中 |
| OverlayNet 透明截获 | VM 已实现 / host 规划中 | libkrun virtio-net + smoltcp 已在 Linux 与 Apple Silicon macOS 上不可绕过地接管 IPv4 TCP/DNS；Linux host netns/seccomp driver 仍在规划中；见[设计](overlaynet.md) |
| 竞品与系统比较 | 参考 | 为后续设计提供输入，不构成产品承诺 |

## 设计原则

1. 保持用户编程模型小而且与能力匹配。
2. 当子系统需要列式存储时，使用 Lance 作为耐久基线。
3. 分离控制面、数据移动和用户执行。
4. 明确失败和恢复语义，不暗示 exactly-once 保证。
