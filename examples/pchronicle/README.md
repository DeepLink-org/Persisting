# pChronicle：Dataset 管理与分析

首次上手请运行 `pchronicle onboard`；以下示例直接使用 `pchronicle` 产品命令和
[`examples/data`](../data/) 中的确定性 Dataset。

| 示例 | 可复现结论 |
|---|---|
| [01-dataset-lifecycle](01-dataset-lifecycle/) | 隔离完成 Dataset 导入、检查、查询、定位与严格导出 |
| [02-built-in-analysis](02-built-in-analysis/) | 对三种格式运行稳定的内置分析并定位指定 Step |
| [03-cross-dataset-sql](03-cross-dataset-sql/) | 命名挂载支持跨三个独立 Dataset 执行 SQL |
| [04-storage-query-performance](04-storage-query-performance/) | 报告 JSON/Lance 体积、压缩比、查询性能和生命周期延迟 |
| [05-format-roundtrip](05-format-roundtrip/) | 严格 ATIF 导入导出后，统一 JSON 格式化的输出按字节相等 |
| [06-query-openai-actf-directly](06-query-openai-actf-directly/) | OpenAI Messages 与 ACTF Dataset 可直接映射为统一 SQL 表 |

运行全部示例：

```bash
just examples-pchronicle
```

默认输出只显示关键事实和结论；每次运行的完整 stdout/stderr 保存在对应场景的
`.work/run.*`。设置 `PCHRONICLE_EXAMPLE_VERBOSE=1` 可在终端同时展开原始日志。
