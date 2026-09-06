# pPilot

**大规模、可恢复的 Run 生产。**

pPilot 把 Run 模型从一次执行扩展到一组有界任务。它负责 planning、有界并发、
lease 与 fencing 决策、基础设施重试与恢复、reconciliation、持久结果发布，以及
task 到 Run 的映射。

它不会重新定义 Agent runtime：每个任务仍然是独立的
[pVisor Run](../pvisor/concepts/run-model.md)，由独立的 `pvisor` 二进制执行。

| 命令 | 负责 |
| --- | --- |
| `ppilot run` | 以可恢复的方式执行 `plan()` / `execute(item)` 工作负载 |
| `ppilot produce` | 从流式 planner 创建彼此独立的 pVisor Run |

## 从这里开始

- [快速开始](get-started.md) — 五分钟跑完第一个并行 plan
- [编排多个 Agent Run](guides/orchestrate.md) — planning、worker、resume 与 sink
- [编排架构](design/orchestration.md) — lease、fencing 与恢复保证
- [pPilot CLI 参考](reference/cli.md) — 精确的标志与退出行为
