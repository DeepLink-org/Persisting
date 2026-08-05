# 可复现示例

仓库里的 [`examples/`](https://github.com/DeepLink-org/Persisting/tree/main/examples)
按产品问题组织。每个 `run.sh` 都是平铺直叙的产品命令：先清理 `.work/`，再运行
pVisor 或 pPilot，最后直接打印生成的文件、Bundle、报告或查询结果。pChronicle 示例
统一通过 `ppilot chronicle` / `ppilot query` 进入，不调用内部 Rust example。

## 运行方式

```bash
just examples                 # 全部（pvisor + pchronicle + ppilot）
just examples-pvisor          # 只跑四组 pVisor 示例
just examples-pchronicle      # 只跑 pChronicle
just examples-ppilot          # 只跑 pPilot
```

所有入口都会增量编译并使用 release Rust targets，之后复用 Cargo 缓存。需要 macOS
或 Linux、Cargo、Python 3 和常见 POSIX 工具（`jq`、`awk`、`curl`）。

## pVisor：轻量级隔离与 Run 管控

这组示例逐项测量事务工作区、changeset、显式网络代理和 Gateway。文件系统示例需要
macOS 的 macFUSE 或 Linux 的 FUSE3。

| 示例 | 可复现结论 | 相关指南 |
|---|---|---|
| [01-filesystem-isolation](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/01-filesystem-isolation) | Agent 写入 upper，lower 在 apply 前保持不变 | [pVisor CLI](../design/cli-pvisor.md) |
| [02-changeset-management](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/02-changeset-management) | changeset 可 review，并可分别 apply 或 drop | [pVisor CLI](../design/cli-pvisor.md) |
| [03-network-isolation](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/03-network-isolation) | 可运行的 CLI/TOML walkthrough：allowlist、deny、deny-all、CIDR/端口/transport、限速及 cooperative 边界 | [OverlayNet 设计](../design/overlaynet.md) |
| [04-gateway-llm-control](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/04-gateway-llm-control) | Gateway 路由并捕获两次 OpenAI-compatible 调用 | [Capture 指南](capture.md) |

这里的"轻量级隔离"特指事务工作区和 cooperative proxy 覆盖的数据面；Run Bundle 会
如实报告 Host executor 仍可访问工作区外路径、直接 socket 仍可绕过显式代理。
[04-gateway-llm-control](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/04-gateway-llm-control)
自带 mock OpenAI-compatible 模型和双轮 Agent，[Capture 指南](capture.md) 的本地
walkthrough 就是基于它。

## pPilot：批量编排与轨迹处理

这组示例覆盖 pPilot 的四个公开工作模式，默认使用本地 Pulsing workers，不要求
`torchrun` 或多节点环境。

| 示例 | 可复现结论 | 相关指南 |
|---|---|---|
| [01-run](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/01-run) | `plan()` / `execute()` 任务被并发执行并写入 durable sink | [快速开始](../quickstart.md) 第 3 步 |
| [02-produce](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/02-produce) | Python planner 生成多个独立、可审查的 pVisor Run | [pPilot CLI](../design/cli-ppilot.md) |
| [03-process](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/03-process) | Python map/reduce 在确定性 ATIF shards 上得到全局结果 | [pPilot CLI](../design/cli-ppilot.md) |
| [04-analysis](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/04-analysis) | 同一条 SQL 在平衡 ATIF shards 上并行执行并合并结果 | [pPilot CLI](../design/cli-ppilot.md) |

CLI 的正式命令名是 `produce`；它对应"生产一批轨迹 Run"的模式，不是 `product`。

## pChronicle：轨迹存储与分析

这组示例使用同一套确定性 ATIF corpus，分别测量物理体积、分析速度和跨格式 SQL
结果一致性。体积和速度结论都限定在脚本打印的数据规模、查询与当前机器；示例不会
宣称 Lance 在任意数据分布和任意查询上必然更小或更快。

| 示例 | 可复现结论 | 相关指南 |
|---|---|---|
| [01-atif-import-compression](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/01-atif-import-compression) | pPilot 导入后直接报告占用比例、空间节省和压缩倍数 | [轨迹格式](../design/trajectory-format.md) |
| [02-lance-vs-atif-speed](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/02-lance-vs-atif-speed) | 量化 pPilot CLI 导入、替换及 Lance/ATIF 冷查询延迟 | [轨迹存储](../design/trajectory.md) |
| [03-analyze-lance-and-atif](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/03-analyze-lance-and-atif) | pPilot 明确报告同一 SQL 的跨后端一致性 | [pPilot CLI](../design/cli-ppilot.md) |

## 前置条件

- macOS 或 Linux；Windows 暂不支持
- Cargo（首次运行按需编译）
- Python 3、`jq`、`awk`、`curl`
- pVisor 文件系统示例需要 macFUSE（macOS）或 FUSE3（Linux）
- 网络示例与 Gateway 示例使用本机 loopback，不需要外部网络

需要 CLI 时先按[安装指南](../installation.md)装好组件集，或 `just install-cli`。
