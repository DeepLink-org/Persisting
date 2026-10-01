# Persisting Examples

**按产品问题组织的可复现 CLI 示例，而不是按 API 罗列。**

每个 `run.sh` 管理自己的 `.work/`、运行产品命令、直接打印生成的文件与报告。
运行后可继续检查 `.work/`。这里不拥有产品实现；pChronicle
的行为以文档站和对应 crate 为准。

## pChronicle

[`data/`](data/) 提供可直接传给 `pchronicle` 的 ATIF、OpenAI Messages 和
ACTF 小型确定性 Dataset，用于手动体验和 CLI 集成测试。

| 示例 | 指标 |
|---|---|
| [2.1 Dataset 生命周期](pchronicle/01-dataset-lifecycle/) | import、ls/stats、query、find、严格 export 的完整路径 |
| [2.2 内置分析与定位](pchronicle/02-built-in-analysis/) | overview、agents、models、tools 与 Step 定位 |
| [2.3 跨 Dataset SQL](pchronicle/03-cross-dataset-sql/) | 三个命名 Dataset 的统一 SQL 查询 |
| [2.4 存储与查询性能](pchronicle/04-storage-query-performance/) | JSON/Lance 体积、压缩比、查询比率与生命周期延迟 |
| [2.5 外围格式往返](pchronicle/05-format-roundtrip/) | 严格 ATIF 往返后的 JSON 数据模型相等 |
| [2.6 直接查询 OpenAI/ACTF](pchronicle/06-query-openai-actf-directly/) | 两种交换格式直接映射为统一逻辑表 |

## Run

```bash
just examples
just examples-pchronicle
```

这些入口统一增量编译并使用 release targets，之后复用 Cargo 缓存。需要
macOS/Linux、Cargo、Python 3、`jq`、`awk`、`curl` 和常见 POSIX 工具。

## Links

- [Reproducible examples](../docs/src/en/project/examples.md)
- [pChronicle get started](../docs/src/en/pchronicle/get-started.md)
