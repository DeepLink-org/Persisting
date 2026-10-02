# 系统架构

pChronicle 库拥有轨迹模型、格式转换、发现、Lance 存储、Snapshot 和有界查询。
CLI 拥有导入导出编排与 loopback Warehouse API，Web UI 使用共享模型和 API 契约。

Storyline 文档直接写入存储。发布表版本与写入租约保证一致性，查询读取固定来源版本。
Dataset、Source 与 revision 身份让转换结果可追溯。

[pChronicle architecture](../pchronicle/design/architecture.md)
