# pPilot：规模化 Run 生产

**问题：pPilot 的两个公开工作模式能否用确定性脚本复现？可复现结论：`run` 把 `plan()` / `execute()` 写入 durable sink；`produce` 生成可审查的独立 pVisor Run。**

这组示例覆盖 pPilot 的两个公开工作模式。每个 `run.sh` 直接执行一种模式，并打印
durable sink 或 Run Bundle。这里不拥有编排实现或 pVisor 隔离后端。

| 示例 | 可复现结论 |
|---|---|
| [01-run](01-run/) | `plan()` / `execute()` 任务被并发执行并写入 durable sink |
| [02-produce](02-produce/) | Python planner 生成多个独立、可审查的 pVisor Run |

CLI 的正式命令名是 `produce`；它对应“生产一批轨迹 Run”的模式，不是 `product`。
这些示例默认使用本地 Pulsing workers，不要求 `torchrun` 或多节点环境。

## Run

```bash
just examples-ppilot
```

## Links

- [Reproducible examples](../../docs/src/project/examples.md)
- [Orchestrate many Agent Runs](../../docs/src/ppilot/guides/orchestrate.zh.md)
- [pPilot CLI](../../docs/src/ppilot/reference/cli.zh.md)
