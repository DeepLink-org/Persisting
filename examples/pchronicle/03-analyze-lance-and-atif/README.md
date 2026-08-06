# 2.3 分析 Lance 与 ATIF 格式轨迹

问题：用户是否能用同一条 SQL 分析 ATIF JSONL 和 pChronicle Lance？

脚本默认生成 64 组、512 条 source-text-diversified ATIF trajectory，通过
`ppilot chronicle import` 导入 Lance，然后通过 `ppilot query` 对两个 backend 执行两类
SQL：全表 `GROUP BY source` 和定位到最后一个长 Storyline 的 `session_id + step range`
选择性查询。只有两类 JSONL 查询结果都逐字相同时才通过；所有 pChronicle 操作都通过
pPilot 产品 CLI 完成。

```bash
./run.sh
```

输出包括两类结果、结果行数与总 step 数，并以明确的 `Conclusion: PASS/FAIL` 和
`RESULT benchmark=query_equivalence ...` 报告查询是否一致。每类查询默认执行 10 轮，
且每轮交替 ATIF/Lance 的先后顺序，验证输出稳定并给出平均延迟和速度比。可通过
`PCHRONICLE_EXAMPLE_SCALE` 和 `PCHRONICLE_QUERY_ITERS` 调整规模与次数。这里包含进程
启动、DataSource 打开、SQL 计划和执行，不代表常驻服务的 warm query 延迟。
