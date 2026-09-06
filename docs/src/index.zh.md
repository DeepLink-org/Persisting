---
title: 从这里开始
sidebar_label: 从这里开始
---

# 从这里开始

Persisting 提供两条独立的产品路径，请按当前任务选择入口：

- [使用 pVisor 安全运行 Agent](pvisor/get-started.md)：暂存 workspace 修改，检查 Run Bundle，只应用你批准的内容。
- [使用 pChronicle 探索持久历史](pchronicle/get-started.md)：打开 Dataset，执行只读查询，并追踪 Source lineage。
- [理解产品边界](overview.md)：了解执行与历史如何连接，同时保持边界清晰。

如果你正在评估 Persisting，先阅读[选择工作流](overview.md)，再进入对应的产品快速开始。

## 完成第一次 walkthrough 后你会得到什么

- **pVisor** walkthrough 会以停止的 Run、可读取的 Run Bundle，以及明确的 apply 或 drop
  决策结束。只有你选择 apply，项目才会收到 staged Effect。
- **pChronicle** walkthrough 会以一次只读 Dataset 查询结束，并明确区分正在读取的 Dataset、
  Source 和 Snapshot。

开始时不需要同时使用两个产品。只有在需要把执行证据与持久轨迹历史关联起来时，才配置 capture 交接。

## 开始前准备

先阅读[安装指南](installation.md)安装 CLI。如果你有本地项目和 Agent 命令，从 pVisor 开始；
如果已经有轨迹数据，或只想体验临时 onboarding Dataset，从 pChronicle 开始。
