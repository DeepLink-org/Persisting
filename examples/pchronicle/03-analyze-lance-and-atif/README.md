# 2.3 三条轨迹分析路径的结果与性能口径

问题：raw Python JSON、pChronicle 直接 JSON 和 pChronicle Lance 是否能得到相同结果，
以及怎样在同一基线上比较它们？

脚本默认生成 64 组、512 条 source-text-diversified ATIF trajectory，通过
`ppilot chronicle import` 导入 Lance，然后执行两类逻辑查询：全表 `GROUP BY source`
和定位到最后一个长 Storyline 的 `session_id + step range` 选择性查询。Python 基线用
`json.loads` 加手写循环；两条 pChronicle 路径用同一条 SQL 分别查询 ATIF 和 Lance。
只有三条路径的 JSONL 结果在 JSON 语义上都相等时才通过。

```bash
./run.sh
```

输出包括结果、总 step 数、median、p95 和相对 Python 的速度。每类查询默认执行 10 轮，
三条路径轮换先后顺序。比值始终是 `Python median / measured-path median`，大于 1 才表示
pChronicle 更快。可通过 `PCHRONICLE_EXAMPLE_SCALE` 和 `PCHRONICLE_QUERY_ITERS` 调整
规模与次数。计时包含独立进程启动以及各自的 parse/open/query，不代表常驻服务延迟。
