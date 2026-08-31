# pVisor：轻量级隔离与 Agent Run 管控

**问题：pVisor 的事务工作区、changeset、显式网络代理和 Gateway 能否用 `run.sh` 定量复现？可复现结论：四个场景分别断言 lower/upper、review/apply/drop、cooperative proxy 边界，以及 Gateway 捕获计数。**

这组示例依次展示 pVisor 的事务工作区、changeset、显式网络代理和 Gateway。每个
`run.sh` 只保留场景准备、pVisor 命令和产物展示；对应的 `test.sh`
调用同一个 `run.sh`，再对 lower/upper、Run Bundle、日志或 AgenticMD 执行回归断言。
这里不拥有隔离后端或 Gateway 实现。

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

## Run

```bash
just examples-pvisor
just examples-pvisor-filesystem  # 01/02，需要 FUSE
just examples-pvisor-portable    # 03/04，普通 CI runner
just example-pvisor 03-network-isolation
```

`examples/pvisor/run.sh` 可批量演示场景，`examples/pvisor/test.sh` 可批量验证场景。两者都
能通过 `PVISOR_BIN` 复用已经构建好的 binary，并通过 `WORK_ROOT` 把临时产物放到指定目录。

## Links

- [Reproducible examples](../../docs/src/project/examples.md)
- [pVisor get started](../../docs/src/pvisor/get-started.zh.md)
- [Isolation architecture](../../docs/src/pvisor/design/isolation.zh.md)
