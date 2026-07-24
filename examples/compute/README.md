# Persisting compute

一个 Python 文件：`plan()` 产出任务，`execute(item)` 处理一条。

```bash
python3 examples/compute/plan_simple.py --n 2
persisting compute examples/compute/plan_simple.py -w 4 -- --n 2
persisting compute examples/compute/plan_simple.py -w 4 --sink /tmp/run1 --resume -- --n 100
torchrun --nproc_per_node=8 -- persisting compute examples/compute/plan_simple.py -- --n 2
```

完整用法：[Compute 快速上手](../../docs/src/guide/compute_quickstart.zh.md)  
架构：[Compute 架构](../../docs/src/design/compute_control_plane.zh.md)
