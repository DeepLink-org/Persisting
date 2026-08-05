# pChronicle：轨迹存储与分析

这组示例使用同一套确定性 ATIF corpus，分别测量物理体积、分析速度和跨格式 SQL
结果一致性。

| 示例 | 可复现结论 |
|---|---|
| [01-atif-import-compression](01-atif-import-compression/) | 直接报告占用比例、空间节省和压缩倍数 |
| [02-lance-vs-atif-speed](02-lance-vs-atif-speed/) | 直接总结构建、冷查询、点查、增量替换和 warm SQL 收益与边界 |
| [03-analyze-lance-and-atif](03-analyze-lance-and-atif/) | 明确报告同一条 SQL 的跨后端一致性结论 |

体积和速度结论都限定在脚本打印的数据规模、查询与当前机器；示例不会宣称 Lance 在
任意数据分布和任意查询上必然更小或更快。
