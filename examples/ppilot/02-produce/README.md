# 3.2 pPilot produce

问题：Python planner 能否流式生成多个独立 pVisor Run，并保留可审查的 Run Bundle？

`production.py` 生成 3 条 shell Run。脚本关闭与本问题无关的 Gateway capture，以 2 路
并发执行，然后打印 production report 和每个 Run Bundle 的 lineage。

```bash
./run.sh
```

预期：3 条 Run 全部完成，生成 3 个 `run-bundle.json`，每个 Bundle 都带有 batch id。
