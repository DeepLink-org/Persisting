# 编排多个 Agent Run

`pPilot` 把 Run 模型从一次执行扩展到一组有界任务。它负责 planning、并发、lease、
基础设施故障重试、持久结果发布和恢复。

它不会重新定义 Agent runtime；每个任务仍然是独立 Run。

## 定义任务

创建 `plan.py`：

```python
def plan():
    for value in range(6):
        yield {"id": f"square-{value}", "value": value}


def execute(item):
    value = item["value"]
    return {"square": value * value}
```

稳定 ID 很重要：retry 与 reconciliation 依靠它识别同一个逻辑任务。

## 使用有界并发运行

```bash
ppilot run plan.py --workers 2 --per-worker 2 --sink ./results
```

`--workers` 与 `--per-worker` 限制 active work；`--sink` 启用 durable result journal
和 lease fencing。

## 检查持久结果

```bash
cat ./results/ready.ndjson
```

基础设施故障可以重试；业务错误会被报告，而不是静默重试。Reconciler 修复结果发布
附近已经支持的崩溃窗口。

## 显式处理外部 Effect

Lease fencing 保护结果所有权，但不能让任意外部 API 自动拥有 exactly-once 语义。请把
稳定 task ID 用作幂等键，或者让外部操作支持 transaction 或 compensation。

## 继续进入历史层

Result sink 不是轨迹历史。应在每个 Run 中捕获 Agent event，再使用
[pChronicle](../../pchronicle/get-started.md)跨 Run 检查。

完整可运行编排案例见[可复现示例](../../project/examples.md)。
