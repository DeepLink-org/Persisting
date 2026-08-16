# pChronicle

**pChronicle 是 Persisting 的结构化轨迹与 Dataset 数据层。** 它发现本地存储或 S3 上的
原生及受支持外部 Source，保留 canonical fact 与来源，提供规范化 Run 视图，并支持有界
查询、分析、revision lineage 与格式交换。

![pChronicle 产品边界](../assets/diagrams/persisting/pchronicle-product.svg)

## pChronicle 负责什么

- Dataset 与 Source discovery；
- 不可变 Catalog Snapshot 的成员和 Source version 描述；
- Canonical event 存储与 Run 终态事实；
- 规范化的 `runs`、`steps` 与 `tool_calls` 查询视图；
- Storyline、ATIF、ACTF 与 OpenAI Messages 的交换边界；
- AgenticMD 非权威人读 projection；
- 派生数据的 revision lineage。

pChronicle 不执行或调度 Agent。它从 runtime event 变成持久历史的位置开始工作。

## 提出第一个问题

```bash
pchronicle ls examples/data/atif
pchronicle analysis overview examples/data/atif
pchronicle query examples/data/atif \
  'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source'
```

## 按目的阅读 pChronicle

| 目标 | 文档 |
| --- | --- |
| 查询第一个 Dataset | [Get Started](get-started.md) |
| 理解 Dataset、Source、event 与 projection | [Concepts](concepts/index.md) |
| 完成常见轨迹数据工作流 | [Guides](guides/index.md) |
| 检查存储与 Catalog 机制 | [Design](design/index.md) |
| 查找命令、schema 与格式 | [Reference](reference/index.md) |

理解 Run 如何执行和捕获，请从 [pVisor](../pvisor/index.md)开始。
