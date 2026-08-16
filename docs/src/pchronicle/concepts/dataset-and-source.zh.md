# Dataset、Source 与 Snapshot

pChronicle 是 path-first 的系统。它保留轨迹数据的物理来源，不把每条轨迹都隐藏在一个
全局数据库 ID 后面。

## Dataset

Dataset 是以规范化本地路径或对象存储 URI 为根的逻辑查询空间。URI 就是它的身份；
Warehouse mount name 只是 SQL 别名，不改变身份。

Dataset 是 discovery、snapshot、query 与 exchange 的边界。它不声称每个预期的外部任务
都产生了 Source。pChronicle 报告它能够发现和固定的 Source；它不会推断未报告的轨迹。

## Source

Source 是 Dataset 中能够独立发现和版本化的最小轨迹表示。它可以是 canonical event store、
Storyline projection 或支持的交换文件。每一行规范化数据都保留 `source_path`，因此外部 ID
仍是 Source-local，冲突不会被隐藏。

实体的完整地址是：

```text
(dataset_uri, source_path, entity_kind, original_id)
```

## Catalog Snapshot

Catalog Snapshot 固定一次操作使用的 Source 成员和版本引用，保证一次查询不会在扫描中途
静默切换版本；它不声称互不相关的 Source 来自同一个全局时刻。

通过 [Dataset 工作流](../guides/discover-and-query.md)检查这些对象；实现机制见
[Dataset Catalog 设计](../design/catalog.md)。
