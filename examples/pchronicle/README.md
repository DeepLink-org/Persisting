# pChronicle：Dataset 管理与分析

**问题：产品 CLI 能否在确定性 Dataset 上走完导入、分析、跨库查询与格式往返？可复现结论：六个场景各自打印可核对的事实，并由 `just examples-pchronicle` 统一构建并验证。**

首次上手请运行 `pchronicle onboard`。以下示例直接使用 `pchronicle` 产品命令和
[`examples/data`](../data/) 中的确定性 Dataset。这里不拥有存储引擎或 CLI 实现。

| 示例 | 可复现结论 |
|---|---|
| [01-dataset-lifecycle](01-dataset-lifecycle/) | 隔离完成 Dataset 导入、检查、查询、定位与严格导出 |
| [02-built-in-analysis](02-built-in-analysis/) | 对三种格式运行稳定的内置分析并定位指定 Step |
| [03-cross-dataset-sql](03-cross-dataset-sql/) | 命名挂载支持跨三个独立 Dataset 执行 SQL |
| [04-storage-query-performance](04-storage-query-performance/) | 报告 JSON/Lance 体积、压缩比、查询性能和生命周期延迟 |
| [05-format-roundtrip](05-format-roundtrip/) | 严格 ATIF 导入导出后，统一 JSON 格式化的输出按字节相等 |
| [06-query-openai-actf-directly](06-query-openai-actf-directly/) | OpenAI Messages 与 ACTF Dataset 可直接映射为统一 SQL 表 |

## Run

```bash
just examples-pchronicle
```

默认输出只显示关键事实和结论；每次运行的完整 stdout/stderr 保存在对应场景的
`.work/run.*`。设置 `PCHRONICLE_EXAMPLE_VERBOSE=1` 可在终端同时展开原始日志。

## Links

- [Reproducible examples](../../docs/src/project/examples.md)
- [pChronicle get started](../../docs/src/pchronicle/get-started.zh.md)
- [Discover and query](../../docs/src/pchronicle/guides/discover-and-query.zh.md)
- [`examples/data`](../data/README.md)
