# Snapshot 与 Source 发现

`DatasetCatalogSnapshot` 固定一个或多个命名 Dataset 挂载下发现的 Source 与版本，
在存储和 DataFusion 之间提供读取边界，显式刷新时重建。它与负责授权、解析路径的
可选 Directory 分离。

## 发现与身份

Dataset 是本地路径或对象存储 URI。Source 包括 Storyline store、Compact JSONL
Dataset 和支持的交换文件。`chronicle.manifest` sidecar 分类嵌套 Dataset 并缓存
聚合统计。递归发现不进入 Lance 内部文件。

实体身份为 `(Dataset 路径, Source 路径, 实体类型, 原始 ID)`。不同 Source 的
外部 ID 不自动合并。行保留 `_file_` 与 `document_id`，命名空间别名支持跨挂载
联查。

发现限制文件数、字节量、解析并发并明确错误策略。本地文件固定身份与指纹，远程
对象在可用时固定版本或 ETag。Storyline store 固定已发布 generation 和精确表版本。
独立 Source 不构成全局事务。

## 延迟查询

```text
挂载 → 发现并固定版本 → Dataset/_file_ 裁剪 → 延迟打开 Source
     → 规范化关系表 → 有界只读 DataFusion 查询
```

Lazy source 在 Snapshot 内复用首次解析结果或失败。Storyline 提供 `runs`、
`steps` 与 `tool_calls`；`trajectories` 是兼容视图，`sources` 描述物理来源。
交换文件解码后使用同一规范化 schema，Compact JSONL 暴露记录关系。
谓词与列下推遵守具体 provider 的语义。

Warehouse 先构建替换 Snapshot 再切换读者，摘要和路由缓存绑定 Snapshot
 generation。刷新失败保留旧 Snapshot。CLI 查询与导出使用固定 Source 版本。

参见 [Dataset、Source 与 Snapshot](../concepts/dataset-and-source.md)、
[查询模型](../reference/query-model.md) 与 [Storyline Lance](storyline-lance.md)。
