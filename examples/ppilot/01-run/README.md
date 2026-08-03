# 3.1 pPilot run

问题：一个流式 Python plan 能否通过多个 worker 执行，并把终态结果写入结果 journal？

`plan.py` 产生 6 个稳定 task id，`execute()` 返回每个输入的平方。脚本使用 2 个
worker、每个 worker 2 个 slot，然后检查成功数、失败数、结果总和和实际 worker 数。

```bash
./run.sh
```

预期：6 个任务全部成功，平方和为 55，并至少使用 2 个 worker slot。
