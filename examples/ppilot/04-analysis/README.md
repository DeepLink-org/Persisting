# 3.4 pPilot analysis

问题：pPilot 能否自动平衡 ATIF 数据，并在每个 shard 上执行同一条 pChronicle SQL？

脚本把仓库内固定的 8 条 ATIF fixture 分成 3 个 shard，运行 `analysis.sql`，再打印合并
结果和 analysis report。SQL 以 session 分组，因此 shard 结果可以直接拼接。

```bash
./run.sh
```

预期：8 行 session 结果覆盖 118 个 step，shard 大小为 3、3、2。
