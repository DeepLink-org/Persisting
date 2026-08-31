# 开始使用 pPilot

本页走最短的已验证 pPilot 闭环：一份流式 Python plan，由多个 worker 执行，
终端结果写入 durable sink。

## 安装

`ppilot` 与 `pvisor`、`pchronicle` 装在同一个 Python wheel 里：

```bash
pip install persisting[lance]
ppilot --version
```

从源码 checkout 时，`just install-cli` 会安装同一组组件。

## 定义工作

创建 `plan.py`：

```python
def plan():
    for value in range(6):
        yield {"id": f"square-{value}", "value": value}


def execute(item):
    return {"square": item["value"] ** 2}
```

`plan()` 产出带稳定 `id` 的工作项；`execute(item)` 处理其中一项。稳定身份让
被中断的作业可以 resume，而不重复已完成的工作。

## 运行

```bash
ppilot run plan.py --workers 2 --per-worker 2 --sink ./results --results ndjson
```

## 核对持久结果

```bash
cat ./results/ready.ndjson
```

预期：六条结果记录，每条任务一条，平方值为 0、1、4、9、16、25（合计 55）。
脚本化版本见
[`examples/ppilot/01-run/`](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/01-run)。

## 接下来去哪

- [编排多个 Agent Run](guides/orchestrate.md) — resume、重试与生产 sink
- [pPilot CLI 参考](reference/cli.md) — `run` 与 `produce` 标志
