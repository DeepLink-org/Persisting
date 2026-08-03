# 2.3 分析 Lance 与 ATIF 格式轨迹

问题：用户是否能用同一条 SQL 分析 ATIF JSONL 和 pChronicle Lance？

脚本生成 4 组 ATIF fixtures，导入 Lance，然后通过 `ppilot query` 对两个 backend 执行
相同的 `GROUP BY source`。只有两份 JSONL 查询结果逐字相同时才通过。

```bash
./run.sh
```

输出包括结果行数与总 step 数。
