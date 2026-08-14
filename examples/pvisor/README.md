# pVisor：轻量级隔离与 Agent Run 管控

这组示例依次展示 pVisor 的事务工作区、changeset、显式网络代理和 Gateway。每个
`run.sh` 都直接执行 pVisor 命令，并打印 lower/upper、Run Bundle 或 AgenticMD 等产物。

| 示例 | 可复现结论 |
|---|---|
| [01-filesystem-isolation](01-filesystem-isolation/) | Agent 写入 upper，lower 在 apply 前保持不变 |
| [02-changeset-management](02-changeset-management/) | changeset 可 review，并可分别 apply 或 drop |
| [03-network-isolation](03-network-isolation/) | 三条平铺命令展示 allowlist、deny-all 与 cooperative proxy 的 direct-socket 边界 |
| [04-gateway-llm-control](04-gateway-llm-control/) | Gateway 路由并捕获两次 OpenAI-compatible 调用 |

文件系统示例需要 macOS 的 macFUSE 或 Linux 的 FUSE3。这里的“轻量级隔离”特指
事务工作区和示例中 cooperative public proxy 所覆盖的数据面；直接 socket 可绕过
该代理。Host executor 的完整文件系统与 deny-all 网络边界因平台而异，以 Run Bundle
和 pVisor 隔离文档为准。
