# pChronicle

**pChronicle 是 Persisting 的结构化轨迹与 Dataset 数据层。** 它发现本地存储或 S3 上的
原生及受支持外部 Source；它在存在 canonical event fact 时予以保留，并始终保留 Source
来源。pChronicle 提供规范化 Run 视图，并支持有界查询、分析、revision lineage 与格式交换。

![pChronicle 产品边界](../assets/diagrams/persisting/pchronicle-product.svg)

## pChronicle 负责什么

- Dataset 与 Source discovery；
- 不可变 Catalog Snapshot 的成员和 Source version 描述；
- Canonical event 存储与 Run 终态事实；
- 规范化的 `runs`、`steps` 与 `tool_calls` 查询视图；
- ATIF、ACTF 与 OpenAI Messages 的 import 边界；
- 上述格式与 Storyline JSON 的 export 边界；
- AgenticMD 非权威人读 projection；
- 派生数据的 revision lineage。

pChronicle 不执行或调度 Agent。它的输入包括 canonical runtime-event Source，以及固定版本的
本地或 S3 ATIF、ACTF、OpenAI Messages 与 Storyline Source。外部 Source 会被直接规范化，
不会先变成 canonical runtime event。

## 提出第一个问题

```bash
pchronicle onboard
pchronicle onboard query
```

这些安装后即可运行的 walkthrough 会创建临时示例 Dataset，不要求源码 checkout。

## 按目的阅读 pChronicle

| 目标 | 文档 |
| --- | --- |
| 查询第一个 Dataset | [Get Started](get-started.md) |
| 理解 Dataset、Source、event 与 projection | [Concepts](concepts/index.md) |
| 完成常见轨迹数据工作流 | [Guides](guides/index.md) |
| 检查存储与 Catalog 机制 | [Design](design/index.md) |
| 查找命令、schema 与格式 | [Reference](reference/index.md) |

要了解 Persisting 治理的 capture 如何通过 pVisor 配置后的 Gateway 与 lifecycle-event 路径
进入 pChronicle，请从 [pVisor](../pvisor/index.md)开始。
