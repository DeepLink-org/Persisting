# 3.3 pPilot process

问题：一个 Python map/reduce job 能否在多个确定性 ATIF shard 上得到全局正确结果？

`metrics.py` 的 mapper 分别统计每个 shard 的 trajectory 和 step，reducer 在 driver 上
合并 partials。脚本使用仓库内固定的 8 条 ATIF fixture 和 4 个 mapper。

```bash
./run.sh
```

预期：处理 8 条 trajectory、118 个 step，并收到 4 个互不重叠的 mapper partial。
