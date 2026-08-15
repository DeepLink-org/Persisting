# pVisor 使用指南

先沿着一个 Run 的生命周期阅读，再使用 pPilot 扩展同一套模型。

1. [选择执行环境](execution.md)。
2. [审查并选择性应用 filesystem Effect](review-apply.md)。
3. [控制网络访问](network.md)。
4. [捕获模型流量与轨迹 evidence](capture.md)。
5. [编排多个独立 Run](orchestrate.md)。

文件系统、网络、capture 与 execution provider 的保证彼此独立。请始终通过 Run Bundle
检查当前平台实际安装的机制。
