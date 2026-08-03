# pPilot：批量编排与轨迹处理

这组示例分别覆盖 pPilot 的四个公开工作模式。每个 `run.sh` 只验证一个模式，输出
`RESULT` 与 `CONCLUSION`，指标不满足时返回非 0。

| 示例 | 可复现结论 |
|---|---|
| [01-run](01-run/) | `plan()` / `execute()` 任务被并发执行并写入 durable sink |
| [02-produce](02-produce/) | Python planner 生成多个独立、可审查的 pVisor Run |
| [03-process](03-process/) | Python map/reduce 在确定性 ATIF shards 上得到全局结果 |
| [04-analysis](04-analysis/) | 同一条 SQL 在平衡 ATIF shards 上并行执行并合并结果 |

CLI 的正式命令名是 `produce`；它对应“生产一批轨迹 Run”的模式，不是 `product`。
这些示例默认使用本地 Pulsing workers，不要求 `torchrun` 或多节点环境。
