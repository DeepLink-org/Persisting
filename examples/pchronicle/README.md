# pChronicle：轨迹存储与分析

这组示例使用同一套确定性 ATIF corpus，分别测量物理体积、分析速度和跨格式 SQL
结果一致性。

| 示例 | 可复现结论 |
|---|---|
| [01-atif-import-compression](01-atif-import-compression/) | ATIF 导入三表 Lance 后展示两边的实际物理体积 |
| [02-lance-vs-atif-speed](02-lance-vs-atif-speed/) | 同一结果下量化 Lance 与 ATIF/DataFusion 查询吞吐 |
| [03-analyze-lance-and-atif](03-analyze-lance-and-atif/) | 同一条只读 SQL 对 Lance 和 ATIF 返回相同结果 |

体积和速度结论都限定在脚本打印的数据规模、查询与当前机器；示例不会宣称 Lance 在
任意数据分布和任意查询上必然更小或更快。
