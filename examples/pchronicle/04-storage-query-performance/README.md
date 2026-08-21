# 4. 存储与查询性能对比

这个示例把仓库内 8 份确定性 ATIF fixture 扩展为 512 个文档，比较原始 JSON 与
Lance/DataFusion 路径，并直接打印主要结论：

- JSON 和 Lance 的物理字节数、`JSON/Lance` 大小比及节省或额外开销；
- Dataset 打开、选择性查询和 `GROUP BY` 的耗时比；
- 冷查询、按 Session 点查和单 Storyline 替换耗时。

运行：

```bash
./run.sh
```

脚本会先验证各条查询得到等价结果，但不会对性能比设置硬阈值；耗时会随机器、文件系统
缓存和后台负载变化。可用以下变量缩放本地运行：

```bash
PCHRONICLE_EXAMPLE_BENCH_SCALE=128 \
PCHRONICLE_EXAMPLE_BENCH_ITERS=30 \
./run.sh
```

默认输出是紧凑报告。benchmark 的完整 stdout/stderr 保存在 `.work/run.*`；设置
`PCHRONICLE_EXAMPLE_VERBOSE=1` 可同时展开原始输出。
