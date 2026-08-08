# pChronicle：轨迹存储与分析

这组示例使用确定性的 ATIF corpus 和裁剪格式 fixture，分别测量物理体积、分析速度、
跨格式 SQL 结果一致性、外围格式经 Lance 三表后的恢复保真度，以及直接目录查询。

| 示例 | 可复现结论 |
|---|---|
| [01-atif-import-compression](01-atif-import-compression/) | 直接报告占用比例、空间节省和压缩倍数 |
| [02-lance-vs-atif-speed](02-lance-vs-atif-speed/) | 直接总结构建、冷查询、点查、增量替换和 warm SQL 收益与边界 |
| [03-analyze-lance-and-atif](03-analyze-lance-and-atif/) | 明确报告同一条 SQL 的跨后端一致性结论 |
| [04-point-batch-live-query](04-point-batch-live-query/) | 对比单 step、整轨迹、64-key 批查和运行中 event follow 的延迟与吞吐 |
| [05-format-roundtrip](05-format-roundtrip/) | 用 pPilot 将 OpenAI/ACTF 导入三表 Lance，再恢复并验证 JSON 数据模型相等 |
| [06-query-openai-actf-directly](06-query-openai-actf-directly/) | 直接查询 OpenAI/ACTF 目录，以 `_file_ LIKE` 缩小路径范围，并验证 Lance schema 不变 |

体积和速度结论都限定在脚本打印的数据规模、查询与当前机器；示例不会宣称 Lance 在
任意数据分布和任意查询上必然更小或更快。
