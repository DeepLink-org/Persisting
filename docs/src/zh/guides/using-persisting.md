# 使用 Persisting

选择能回答当前问题的最小工作流，不需要一次采用所有组件。

## 我已经有轨迹数据

从[pChronicle 入门](../pchronicle/get-started.md)开始：打开 Dataset、查看汇总、提出一个
有界 SQL 问题，再定位答案背后的 Evidence。只读流程稳定后，再使用导入导出或服务指南。

## 我需要捕获新的模型请求

使用 [Gateway](../pchronicle/guides/serve-gateway.md) 转发请求并持久化 canonical events。
外部执行组件也可以通过 pChronicle Control 服务提交事件。

## 一套可靠的使用习惯

1. 从一个 Run 或一个 Dataset 开始。
2. 记录完整命令、路径和 provider。
3. 在应用、导出或分享前先审查结果。
4. 让结论始终带有 Source 和 Evidence 位置。
5. 手工路径可重复后，再进入自动化。

实现背景见[设计原则](../system-design/design-principles.md)。
