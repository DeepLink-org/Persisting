# Dataset、Source 与 Snapshot

pChronicle 是 Agent 轨迹存储引擎。Dataset 就是 **path**：保留轨迹数据的物理来源，不把每条
轨迹都隐藏在一个全局数据库 ID 后面。

## Dataset

Dataset 是以规范化本地路径或对象存储 URI 为根的查询空间。path 就是它的身份；
Warehouse mount name 只是 SQL 别名。`@alias` 或 Directory library 名是 locator；解析完成后
引擎打开的是 path。

Dataset 是 discovery、Snapshot、query 与 exchange 的边界。它不声称每个预期的外部任务
都产生了 Source。pChronicle 报告它能够发现和固定的 Source；它不会推断未报告的轨迹。

## Source

Source 是 Dataset 中能够独立发现和版本化的最小轨迹表示。它可以是 canonical event store、
Storyline projection 或支持的交换文件。每一行规范化数据都保留 `source_path`，因此外部 ID
仍是 Source-local，冲突不会被隐藏。

实体的完整地址是：

```text
(path, source_path, entity_kind, original_id)
```

## Snapshot

Snapshot 是打开 path 之后写入与读取之间的同步协议。它固定一次操作使用的 Source 成员和
版本引用，保证一次查询不会在扫描中途静默切换版本；它不声称互不相关的 Source 来自同一个
全局时刻。

Directory 的列表与换票（RFC-0013）不是 Snapshot，只决定调用方可以打开哪些 path。

通过 [Dataset 工作流](../guides/discover-and-query.md)检查这些对象；实现机制见
[Snapshot 设计](../design/catalog.md)。
