# Persisting pPilot

一个 Python 文件：`plan()` 产出任务，`execute(item)` 处理一条。

```bash
python3 examples/ppilot/plan_simple.py --n 2
persisting ppilot examples/ppilot/plan_simple.py -w 4 -- --n 2
persisting ppilot examples/ppilot/plan_simple.py -w 4 --sink /tmp/run1 --resume -- --n 100
torchrun --nproc_per_node=8 -- persisting ppilot examples/ppilot/plan_simple.py -- --n 2
```

完整用法：[pPilot 快速上手](../../docs/src/guide/ppilot.zh.md)

架构：[pPilot 架构](../../docs/src/design/ppilot.md)
