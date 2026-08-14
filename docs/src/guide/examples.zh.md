# 可复现示例

[`examples/`](https://github.com/DeepLink-org/Persisting/tree/main/examples)
按产品命令组织。每个 `run.sh` 清理自己的 `.work/`，并输出持久结果或查询结果。

```bash
just examples
just examples-pvisor
just examples-ppilot
just examples-pchronicle
```

## pVisor

| 示例 | 可复现结论 |
|---|---|
| `01-filesystem-isolation` | 事务工作区隔离 |
| `02-changeset-management` | review、apply 与 drop |
| `03-network-isolation` | 显式代理策略及其边界 |
| `04-gateway-llm-control` | 内嵌 Gateway 路由与捕获 |

## pPilot

| 示例 | 可复现结论 |
|---|---|
| `01-run` | `plan()` / `execute()` 并发执行并写 durable sink |
| `02-produce` | 流式 planner 创建独立 pVisor Run |

## pChronicle

| 示例 | 可复现结论 |
|---|---|
| `05-format-roundtrip` | 严格 ATIF 往返并规范化后按字节比较 |
| `06-query-openai-actf-directly` | 直接对 OpenAI Messages 与 ACTF Dataset 执行 SQL |

pChronicle 示例使用 `examples/data` 中的确定性 fixture。运行要求为 macOS 或 Linux、
Cargo、Python 3 和 `jq` 等常见 POSIX 工具；pVisor 文件系统示例另需 macFUSE 或 FUSE3。
