# pPilot：规模化 Run 生产

这组示例覆盖 pPilot 的两个公开工作模式。每个 `run.sh` 直接执行一种模式，并打印
durable sink 或 Run Bundle。

| 示例 | 可复现结论 |
|---|---|
| [01-run](01-run/) | `plan()` / `execute()` 任务被并发执行并写入 durable sink |
| [02-produce](02-produce/) | Python planner 生成多个独立、可审查的 pVisor Run |

CLI 的正式命令名是 `produce`；它对应“生产一批轨迹 Run”的模式，不是 `product`。
这些示例默认使用本地 Pulsing workers，不要求 `torchrun` 或多节点环境。
