# pVisor 使用指南

先沿着一个 Run 的生命周期阅读，需要时再把执行连接到持久历史。

:::note 先按结果选择
下面每篇指南都围绕一个具体任务展开。先选择当前最需要的结果，并在操作过程中保留 Run Bundle；
它是当前平台实际安装了哪些控制机制的权威记录。
:::

1. [选择执行环境](execution.md)。
2. [审查并选择性应用 filesystem Effect](review-apply.md)。
3. [控制网络访问](network.md)。
4. [捕获模型流量与轨迹 evidence](capture.md)。
5. [在新沙箱中回放并续跑 Agent 轨迹](sandbox-replay.md)。

文件系统、网络、capture 与 execution provider 的保证彼此独立。请始终通过 Run Bundle
检查当前平台实际安装的机制。
