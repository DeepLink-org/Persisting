# 使用 Persisting

选择能回答当前问题的最小工作流，不需要一次采用所有组件。

## 我需要 Agent 安全地修改项目

从[pVisor 入门](../pvisor/get-started.md)开始：在 staged workspace 中运行 Agent，
审查 Run Bundle，再只应用信任的路径。下一次 Run 确实需要时，再增加网络或 provider 控制。

## 我已经有轨迹数据

从[pChronicle 入门](../pchronicle/get-started.md)开始：打开 Dataset、查看汇总、提出一个
有界 SQL 问题，再定位答案背后的 Evidence。只读流程稳定后，再使用导入导出或服务指南。

## 我需要把执行和历史放在一起

当 Run 的生命周期事件需要成为 pChronicle Source 时，使用[pVisor capture](../pvisor/guides/capture.md)。
私有 Run Bundle 仍是本地执行记录；capture 是显式交接，不会隐式复制所有 Artifact。

## 一套可靠的使用习惯

1. 从一个 Run 或一个 Dataset 开始。
2. 记录完整命令、路径和 provider。
3. 在应用、导出或分享前先审查结果。
4. 让结论始终带有 Source 和 Evidence 位置。
5. 手工路径可重复后，再进入自动化。

实现背景见[设计原则](../system-design/design-principles.md)。
