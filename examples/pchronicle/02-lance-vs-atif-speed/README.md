# 2.2 Lance 与 ATIF 分析速度对比

问题：同一个 DataFusion 查询在三表 Lance 与 ATIF MemTable 上，各自吞吐是多少？

脚本运行仓库 benchmark。benchmark 会先验证 Lance、ATIF、JSON scan 和预解析 JSON
得到相同结果，再测量选择性查询与 `GROUP BY`。默认 64 组数据、20 次 warm query。

```bash
./run.sh
SCALE=128 ITERATIONS=50 ./run.sh
PROFILE=release ./run.sh
```

默认 `PROFILE=debug` 以复用日常编译缓存并缩短首次体验；它适合验证方法和比较当前
binary。需要发布构建下的数字时显式使用 `PROFILE=release`，首次编译会明显更久。

结果不预设胜者。`atif_over_lance_time > 1` 表示这次测量中 Lance 用时更少，反之
表示 ATIF 内存表更少；不同 build profile 的数字不能直接横向比较。
