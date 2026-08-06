# 2.2 pChronicle 性能对比

问题：通过 pPilot 产品 CLI 使用 pChronicle 三表 Lance 后，导入、增量替换和冷 SQL
查询分别需要多少时间？

脚本只调用 `ppilot chronicle import` 和 `ppilot query`，并从外部测量完整 CLI
端到端路径，包括进程启动、输入解析、DataSource 打开、SQL 计划和执行。它会先验证
Lance 与 ATIF 返回完全相同的结果，再报告：

- 完整 ATIF 导入和单 Storyline 增量替换延迟；
- 完整 Lance store 与 ATIF JSONL 的物理体积；
- 选择性查询和 `GROUP BY` 的冷 CLI 平均延迟；
- 两种 pPilot query backend 的结果一致性。

和 01 相同，trajectory 结构来自 ATIF fixtures，但 message 内容按固定种子从仓库源码
块中抽取，避免重复 fixture 文本扭曲存储与扫描结果。输出末尾的 `Conclusion` 会直接
总结存储节省、冷查询速度、导入与在线替换延迟。

默认使用 64 组数据和 20 次冷 CLI 查询：

```bash
./run.sh
```

可以直接用环境变量做规模对照：

```bash
PCHRONICLE_BENCH_SCALE=256 PCHRONICLE_BENCH_ITERS=10 ./run.sh
```

脚本通过 `PATH` 使用当前仓库构建的 pPilot，并让 `ppilot query` 根据输入自动识别 ATIF
文件和 Lance store。结果不预设胜者；这里测的是一次命令一次查询的真实冷 CLI 延迟，
不代表常驻进程中的 warm DataFusion 吞吐。示例固定使用 release 版本；不同机器上的
数字仍不能直接横向比较。
