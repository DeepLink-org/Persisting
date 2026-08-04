# Persisting Examples

这里的 example 按产品问题组织，而不是按 API 罗列。每个 `run.sh` 都是平铺直叙的产品
命令：清理 `.work/`、运行命令、直接打印生成的文件与报告。运行后可继续检查 `.work/`。

## 1. pVisor 执行轻量级隔离

| 示例 | 指标 |
|---|---|
| [1.1 文件系统隔离](pvisor/01-filesystem-isolation/) | lower 值、upper 文件数、Bundle changes |
| [1.2 changeset 管理](pvisor/02-changeset-management/) | review/apply/drop 文件数 |
| [1.3 网络系统隔离](pvisor/03-network-isolation/) | proxy allow/deny counters 与边界强度 |
| [1.4 Gateway 捕获与管控 LLM](pvisor/04-gateway-llm-control/) | upstream POST、sink requests、AgenticMD blocks |

## 2. pChronicle 轨迹存储

| 示例 | 指标 |
|---|---|
| [2.1 导入 ATIF 并比较压缩比](pchronicle/01-atif-import-compression/) | ATIF/Lance bytes 与压缩比 |
| [2.2 Lance vs ATIF 分析速度](pchronicle/02-lance-vs-atif-speed/) | 两类查询的 QPS 与耗时比 |
| [2.3 分析 Lance 和 ATIF](pchronicle/03-analyze-lance-and-atif/) | 同 SQL 的结果一致性、行数、step 数 |

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
`just examples-ppilot`。首次运行会按需编译 Rust targets，之后复用 Cargo 缓存。需要
macOS/Linux、Cargo、Python 3、`jq`、`awk`、`curl` 和常见 POSIX 工具；OverlayFS
示例还需要 macFUSE 或 FUSE3。
