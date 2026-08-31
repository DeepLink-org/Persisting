# Persisting Examples

**按产品问题组织的可复现 CLI 示例，而不是按 API 罗列。**

每个 `run.sh` 管理自己的 `.work/`、运行产品命令、直接打印生成的文件与报告。
运行后可继续检查 `.work/`。这里不拥有产品实现；pVisor / pPilot / pChronicle
的行为以文档站和对应 crate 为准。

## pVisor

| 示例 | 指标 |
|---|---|
| [1.1 文件系统隔离](pvisor/01-filesystem-isolation/) | lower 值、upper 文件数、Bundle changes |
| [1.2 changeset 管理](pvisor/02-changeset-management/) | review/apply/drop 文件数 |
| [1.3 pVisor 网络边界](pvisor/03-network-isolation/) | allowlist、deny-all，以及 direct socket 可绕过 cooperative proxy 的边界 |
| [1.4 Gateway 捕获与管控 LLM](pvisor/04-gateway-llm-control/) | upstream POST、sink requests、AgenticMD blocks |

## pChronicle

[`data/`](data/) 提供可直接传给 `pchronicle` 的 ATIF、OpenAI Messages 和
ACTF 小型确定性 Dataset，用于手动体验和 CLI 集成测试。

| 示例 | 指标 |
|---|---|
| [2.1 Dataset 生命周期](pchronicle/01-dataset-lifecycle/) | import、ls/status、query、find、严格 export 的完整路径 |
| [2.2 内置分析与定位](pchronicle/02-built-in-analysis/) | overview、agents、models、tools 与 Step 定位 |
| [2.3 跨 Dataset SQL](pchronicle/03-cross-dataset-sql/) | 三个命名 Dataset 的统一 SQL 查询 |
| [2.4 存储与查询性能](pchronicle/04-storage-query-performance/) | JSON/Lance 体积、压缩比、查询比率与生命周期延迟 |
| [2.5 外围格式往返](pchronicle/05-format-roundtrip/) | 严格 ATIF 往返后的 JSON 数据模型相等 |
| [2.6 直接查询 OpenAI/ACTF](pchronicle/06-query-openai-actf-directly/) | 两种交换格式直接映射为统一逻辑表 |

## pPilot

| 示例 | 指标 |
|---|---|
| [3.1 run](ppilot/01-run/) | 完成/失败数、结果总和、worker slot 数 |
| [3.2 produce](ppilot/02-produce/) | 完成数、Run Bundle 数、lineage 数 |

## Run

```bash
just examples
just examples-pvisor
just examples-pchronicle
just examples-ppilot
```

这些入口统一增量编译并使用 release targets，之后复用 Cargo 缓存。需要
macOS/Linux、Cargo、Python 3、`jq`、`awk`、`curl` 和常见 POSIX 工具；
OverlayFS 示例还需要 macFUSE 或 FUSE3。

## Links

- [Reproducible examples](../docs/src/project/examples.md)
- [pVisor get started](../docs/src/pvisor/get-started.md)
- [pPilot get started](../docs/src/ppilot/get-started.md)
- [pChronicle get started](../docs/src/pchronicle/get-started.md)
