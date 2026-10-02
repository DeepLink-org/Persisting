# RFC-0003：pChronicle 职责

pChronicle 拥有 Storyline 模型、外围格式编解码、物理存储、Source 发现、
Snapshot、DataFusion 查询与 revision lineage。CLI 和 Web 使用这些契约，
不得引入第二套通用轨迹模型或并行存储格式。

CLI 拥有命令、导入导出编排、loopback Warehouse HTTP 服务与静态资源嵌入。
Web 拥有浏览界面。

[Architecture](../pchronicle/design/architecture.md)
