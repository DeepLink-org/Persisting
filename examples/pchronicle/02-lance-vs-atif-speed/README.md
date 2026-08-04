# 2.2 Lance 与 ATIF 分析速度对比

问题：同一个 DataFusion 查询在三表 Lance 与 ATIF MemTable 上，各自吞吐是多少？

脚本运行仓库 benchmark。benchmark 会先验证 Lance、ATIF、JSON scan 和预解析 JSON
得到相同结果，再测量选择性查询与 `GROUP BY`。默认 64 组数据、20 次 warm query。

```bash
./run.sh
```

脚本固定使用 64 组数据、20 次查询和 debug build，以便命令保持具体、容易阅读。
需要其他规模或 release build 时，可以直接修改 `run.sh` 中对应的命令参数。

结果不预设胜者。`atif_over_lance_time > 1` 表示这次测量中 Lance 用时更少，反之
表示 ATIF 内存表更少；不同 build profile 的数字不能直接横向比较。
