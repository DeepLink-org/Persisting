# 复现 Run 生命周期

[`examples/`](https://github.com/DeepLink-org/Persisting/tree/main/examples)
按产品命令组织。每个 `run.sh` 管理自己的 `.work/`，并输出持久结果或查询结果。它们共同
对应使用指南的主线：执行、治理 Effect、检查历史。

```bash
just examples
just examples-pvisor
just examples-pchronicle
```

## pVisor

| 示例 | 可复现结论 |
|---|---|
| `01-filesystem-isolation` | 事务工作区隔离 |
| `02-changeset-management` | review、apply 与 drop |
| `03-network-isolation` | 显式代理策略及其边界 |
| `04-gateway-llm-control` | 内嵌 Gateway 路由与捕获 |

## orchestration layer

| 示例 | 可复现结论 |
|---|---|
| `01-run` | 并发执行 `plan()` / `execute()` 并将结果写入持久化 sink |
| `02-produce` | Python planner 生成多个独立、可审查的 pVisor Run |

## pChronicle

| 示例 | 可复现结论 |
|---|---|
| `01-dataset-lifecycle` | 导入、检查、查询、定位并严格导出 Dataset |
| `02-built-in-analysis` | 汇总 Sources、Agents、Models、工具并定位 Step |
| `03-cross-dataset-sql` | 对三个命名 Dataset 挂载执行跨 Dataset SQL |
| `04-storage-query-performance` | 比较 JSON/Lance 体积、压缩比、查询比率与生命周期延迟 |
| `05-format-roundtrip` | 严格 ATIF 往返并规范化后按字节比较 |
| `06-query-openai-actf-directly` | 直接对 OpenAI Messages 与 ACTF Dataset 执行 SQL |

pChronicle 示例使用 `examples/data` 中的确定性 fixture。默认只输出紧凑报告；完整命令
stdout/stderr 保存在各场景的 `.work/run.*`，也可通过
`PCHRONICLE_EXAMPLE_VERBOSE=1` 在终端展开。运行要求为 macOS 或 Linux、Cargo、
Python 3 和 `jq` 等常见 POSIX 工具；pVisor 文件系统示例另需 macFUSE 或 FUSE3。
`just examples-pvisor-filesystem` 运行依赖 FUSE 的 01/02；
`just examples-pvisor-portable` 运行不需要 FUSE 的 03/04。

建议从 `pvisor/01-filesystem-isolation` 开始，再进入 changeset management；需要多 Run
生产时运行 orchestration layer 示例；准备从执行进入历史时，再运行 pChronicle 示例。

任务解释见 [pVisor Guides](../pvisor/guides/index.md)，多 Run 工作流见
批量 Run 工作流见
[pChronicle Guides](../pchronicle/guides/index.md)。示例用于验证产品路径；精确命令语法仍以
各产品 Reference 为准。
