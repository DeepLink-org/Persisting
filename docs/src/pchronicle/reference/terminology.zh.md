# 产品术语

pChronicle 在命令行、Web 界面和任务型文档中统一使用以下词汇：

| 术语 | 含义 |
| --- | --- |
| **Dataset（数据集）** | 一条 Agent 轨迹存储，身份是 **path**（本地路径或对象存储 URI）。 |
| **Source file（源文件）** | Dataset 中的一个输入文件或存储来源。 |
| **Run（运行）** | 一条 Agent 执行记录。Session ID 标识这条记录；可选的 Run ID 用于关联运行时活动。 |
| **Step（步骤）** | Run 中按顺序排列的一条用户、Agent、系统或工具相关记录。 |
| **Event（事件）** | 与 Step 关联的底层记录事件或重建事件。 |
| **Result（结果）** | 查询或分析返回的数据行。 |
| **Snapshot** | 打开 path 之后写入与读取之间的同步协议：有哪些 Source、一次操作各钉在哪个版本。 |

界面使用 **Recorded events（记录事件）** 表示运行时直接捕获的事件，使用
**Reconstructed events（重建事件）** 表示从导入的运行数据推导出的事件。重建事件不会被标成
原始数据。

以下词汇只用于技术文档和 API：

- **Storyline** 是 Run 与 Step 背后的规范化存储和交换模型；
- **Canonical Event** 是只追加的记录事件模型；
- **Directory** 是可选的平台 locator：把名字解析成 path 并换票。CLI 标志和 HTTP 路径仍可能写 `catalog`；
- **Snapshot**（API 里有时仍叫 `DatasetCatalogSnapshot`）钉住 Source 成员与版本，不是 Directory 列表；
- **projection、revision、fragment、column page** 描述存储和一致性机制，不是日常操作入口。

alias（`@name`）、Warehouse mount 名和 Directory library 名都是 locator。解析完成后引擎只看见 path。

为保持兼容，旧 API 路径和 schema 字段可能继续使用技术名称；用户界面遵循上面的简化词表。
