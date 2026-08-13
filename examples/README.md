# Persisting Examples

这里的 example 按产品问题组织，而不是按 API 罗列。每个 `run.sh` 都是平铺直叙的产品
命令：清理 `.work/`、运行命令、直接打印生成的文件与报告。运行后可继续检查 `.work/`。

## 1. pVisor 执行轻量级隔离

| 示例 | 指标 |
|---|---|
| [1.1 文件系统隔离](pvisor/01-filesystem-isolation/) | lower 值、upper 文件数、Bundle changes |
| [1.2 changeset 管理](pvisor/02-changeset-management/) | review/apply/drop 文件数 |
| [1.3 pVisor 网络边界](pvisor/03-network-isolation/) | allowlist、deny-all，以及 direct socket 可绕过 cooperative proxy 的边界 |
| [1.4 Gateway 捕获与管控 LLM](pvisor/04-gateway-llm-control/) | upstream POST、sink requests、AgenticMD blocks |

## 2. pChronicle 轨迹存储

[`data/`](data/) 提供可直接传给 `pchronicle` 的 ATIF、OpenAI Messages 和
ACTF 小型确定性 Dataset，用于手动体验和 CLI 集成测试。

| 示例 | 指标 |
|---|---|
| [2.1 raw JSON 与 Lance 体积](pchronicle/01-atif-import-compression/) | raw JSON 与完整 pChronicle Lance store bytes |
| [2.2 Python JSON 与 pChronicle](pchronicle/02-lance-vs-atif-speed/) | Python 基线、pChronicle JSON、pChronicle Lance 的冷进程查询 |
| [2.3 三路径分析一致性](pchronicle/03-analyze-lance-and-atif/) | 三条路径的语义结果、median/p95 和统一相对值 |
| [2.4 点查、批查与 live follow](pchronicle/04-point-batch-live-query/) | pChronicle API 延迟、CLI batching gain 和运行中 follow |
| [2.5 外围格式往返](pchronicle/05-format-roundtrip/) | pPilot 导入/恢复 OpenAI 与 ACTF 后 JSON 数据模型相等 |
| [2.6 直接查询 OpenAI/ACTF](pchronicle/06-query-openai-actf-directly/) | `_file_` 相对路径、`LIKE` 筛选与 Lance schema 隔离 |
| [2.7 大字段 Blob 外置](pchronicle/07-objects-lance-blob-offload/) | inline/offload 体积、压缩与查询开销 |

## 3. pPilot 批量编排与轨迹处理

| 示例 | 指标 |
|---|---|
| [3.1 run](ppilot/01-run/) | 完成/失败数、结果总和、worker slot 数 |
| [3.2 produce](ppilot/02-produce/) | 完成数、Run Bundle 数、lineage 数 |
| [3.3 process](ppilot/03-process/) | trajectory/step 数、mapper partial 数 |
| [3.4 analysis](ppilot/04-analysis/) | SQL 行数、step 总数、平衡 shard 大小 |

运行全部示例：

```bash
just examples
```

也可以运行 `just examples-pvisor`、`just examples-pchronicle` 或
`just examples-ppilot`。这些入口统一增量编译并使用 release targets，之后复用 Cargo
缓存。需要 macOS/Linux、Cargo、Python 3、`jq`、`awk`、`curl` 和常见 POSIX 工具；
OverlayFS 示例还需要 macFUSE 或 FUSE3。
