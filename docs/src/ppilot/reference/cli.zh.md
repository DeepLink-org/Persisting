# `ppilot` 命令参考

`ppilot` 是可扩展的 Run 生产 CLI。它只暴露两条命令：

```text
ppilot
├── run       execute plan() / execute(item) with durable recovery
└── produce   create independent pVisor Runs from a streaming planner
```

Dataset 发现、SQL、内置分析、find、导入导出与 serving 由
[`pchronicle`](../../pchronicle/reference/cli.md) 负责。

## `run`

```bash
ppilot run plan.py --workers 8 --per-worker 2 --sink ./results
ppilot run plan.py --workers 8 --sink ./results --resume
ppilot run plan.py --check
ppilot run plan.py --pvisor-binary ./target/release/pvisor
```

脚本定义 `plan()` 和 `execute(item)`。pPilot 施加有界并发与 backpressure，
把终端结果写入 durable sink，并用稳定 task 身份做 resume 与 retry。`--check`
校验 plan 和一次样例执行，而不跑完整工作负载。

## `produce`

```bash
ppilot produce production.py --output ./runs --parallelism 8
ppilot produce production.py --output ./runs --parallelism 8 \
  --cluster-network-limit 10mbps -- --dataset train
```

planner 的 `plan()` 可以是同步或异步 iterator。每一项描述一次 Run：

```python
def plan():
    for index in range(100):
        yield {
            "id": f"task-{index:04d}",
            "agent": "codex",
            "command": ["codex", "exec", f"Solve task {index}"],
            "cwd": "/work/eval",
        }
```

每个产出项在 `--output` 下拥有自己的 pVisor workspace。planner 在并发窗口内
流式消费，因此大批次不会完整驻留内存。命令写入 `production-report.json`；
任一 Run 失败会在报告落盘后让命令以失败退出。

`--cluster-network-limit` 把保守的聚合代理速率按请求的 parallelism 均分。
它要求 Gateway capture，且不覆盖绕过显式代理的直接 socket。

## Runtime ownership

两条命令都启动进程内、作业范围的 Supervisor。pPilot 负责 planning、lease、
重试、reconciliation 和收集。pVisor 负责 Run 执行和内嵌 Gateway。pPilot 为
每个 Run 调用一个前台 `pvisor` 进程；两个组件通过 agentctl 共享 Run 与
Supervisor 契约，而不是把 pVisor 链进 pPilot。`--pvisor-binary` 和
`PERSISTING_PVISOR_BIN` 选择显式可执行文件。pChronicle 负责轨迹 Dataset
操作。

可执行文件的 `--help` 是标志与默认值的权威来源。

完整工作流见 [编排多个 Agent Run](../guides/orchestrate.md)，lease 与
reconciliation 见 [pPilot 架构](../design/orchestration.md)，重试身份模型见
[Run、Attempt 与 Effect](../../pvisor/concepts/run-model.md)。
