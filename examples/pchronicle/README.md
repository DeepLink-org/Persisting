# pChronicle：Dataset 管理与分析

首次上手请运行 `pchronicle onboard`；以下示例直接使用 `pchronicle` 产品命令和
[`examples/data`](../data/) 中的确定性 Dataset。

| 示例 | 可复现结论 |
|---|---|
| [05-format-roundtrip](05-format-roundtrip/) | 严格 ATIF 导入导出后，统一 JSON 格式化的输出按字节相等 |
| [06-query-openai-actf-directly](06-query-openai-actf-directly/) | OpenAI Messages 与 ACTF Dataset 可直接映射为统一 SQL 表 |
