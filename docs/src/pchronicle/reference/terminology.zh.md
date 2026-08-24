# 产品术语

pChronicle 在命令行、Web 界面和任务型文档中统一使用以下词汇：

| 术语 | 含义 |
| --- | --- |
| **Dataset（数据集）** | 一组已挂载的 Agent 运行数据。 |
| **Source file（源文件）** | Dataset 中的一个输入文件或存储来源。 |
| **Run（运行）** | 一条 Agent 执行记录。Session ID 标识这条记录；可选的 Run ID 用于关联运行时活动。 |
| **Step（步骤）** | Run 中按顺序排列的一条用户、Agent、系统或工具相关记录。 |
| **Event（事件）** | 与 Step 关联的底层记录事件或重建事件。 |
| **Result（结果）** | 查询或分析返回的数据行。 |

界面使用 **Recorded events（记录事件）** 表示运行时直接捕获的事件，使用
**Reconstructed events（重建事件）** 表示从导入的运行数据推导出的事件。重建事件不会被标成
原始数据。

以下词汇只用于技术文档和 API：

- **Storyline** 是 Run 与 Step 背后的规范化存储和交换模型；
- **Canonical Event** 是只追加的记录事件模型；
- **Dataset Catalog** 是发现和查询已挂载 Dataset 的内部不可变快照；
- **projection、revision、fragment、column page** 描述存储和一致性机制，不是日常操作入口。

为保持兼容，旧 API 路径和 schema 字段可能继续使用技术名称；用户界面遵循上面的简化词表。

