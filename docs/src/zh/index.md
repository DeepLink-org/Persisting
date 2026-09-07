---
title: 从这里开始
sidebar_label: 从这里开始
---

# 从这里开始

Persisting 提供两条独立的产品路径，请按当前任务选择入口：

- [使用 pVisor 安全运行 Agent](pvisor/get-started.md)：在暂存工作区里跑 Agent，检查改动，只把你批准的内容写入项目。
- [使用 pChronicle 探索持久历史](pchronicle/get-started.md)：打开一份轨迹数据，跑只读查询，弄清查的是哪份数据、哪个来源。
- [选择工作流](overview.md)：弄清该走哪条路径，以及执行与历史如何可选地连接。

如果你正在评估 Persisting，先阅读[选择工作流](overview.md)，再进入对应的产品快速开始。

## 完成第一次 walkthrough 后你会得到什么

- **pVisor**：Agent 已停止；改动仍在暂存目录；你明确选择写入项目或丢弃。只有你选择写入，项目才会变化。
- **pChronicle**：你对一份轨迹数据跑过只读查询，并知道查的是哪份数据、哪个来源。

开始时不需要同时使用两个产品。只有在需要把一次执行与持久轨迹关联起来时，再配置 capture 交接。

## 开始前准备

先阅读[安装指南](installation.md)安装 CLI。如果你有本地项目和 Agent 命令，从 pVisor 开始；
如果已经有轨迹数据，或只想体验临时 onboarding Dataset，从 pChronicle 开始。
