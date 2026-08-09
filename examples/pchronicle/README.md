# pChronicle：轨迹存储与分析

这组示例使用确定性的 ATIF corpus 和裁剪格式 fixture，展示 pChronicle 的文件查询、
Lance 存储、跨格式 SQL、外围格式恢复和直接目录查询。

所有查询性能实验使用同一个口径：Python 标准库 `json.loads` 加等价的手写过滤或聚合是
原始 JSON 基线；pChronicle 直接查询 JSON，以及 pChronicle 查询导入后的 Lance，是两条
独立的增强路径。三者先验证语义结果相等，再分别相对 Python 基线报告 median/P95、
输入 rows/s 和独立子进程峰值 RSS；pChronicle JSON 额外报告 source bytes、输入 buffer 峰值
与 projected Arrow bytes。
`speedup_vs_python = Python median / measured-path median`，只有大于 1 才表示更快。

| 示例 | 可复现结论 |
|---|---|
| [01-atif-import-compression](01-atif-import-compression/) | raw JSON bytes 与 pChronicle Lance 完整 store bytes |
| [02-lance-vs-atif-speed](02-lance-vs-atif-speed/) | Python JSON 基线、pChronicle JSON 和 pChronicle Lance 的冷进程查询 |
| [03-analyze-lance-and-atif](03-analyze-lance-and-atif/) | 三条路径执行等价分析并验证语义结果一致 |
| [04-point-batch-live-query](04-point-batch-live-query/) | pChronicle 产品 API 的单查、批查摊销和 event follow 延迟 |
| [05-format-roundtrip](05-format-roundtrip/) | 用 pPilot 将 OpenAI/ACTF 导入三表 Lance，再恢复并验证 JSON 数据模型相等 |
| [06-query-openai-actf-directly](06-query-openai-actf-directly/) | 直接查询 OpenAI/ACTF 目录，以 `_file_ LIKE` 缩小路径范围，并验证 Lance schema 不变 |

查询计时都包含独立进程启动，以及各自的 JSON 解析或 store open、计划和执行。体积和
速度结论限定在脚本打印的数据规模、查询与当前机器；API 摊销、导入成本和存储空间不与
查询加速混成一个指标。

如需观察完整 projected JSON datasource + SQL 查询的 allocation traffic，可运行：

```bash
PCHRONICLE_BENCH_SCALE=128 PCHRONICLE_BENCH_ITERS=30 \
  cargo bench -p persisting-pchronicle --bench json_streaming

PCHRONICLE_BENCH_JSON_SHAPE=array PCHRONICLE_BENCH_SCALE=128 \
  PCHRONICLE_BENCH_ITERS=30 cargo bench -p persisting-pchronicle --bench json_streaming
```

该基准在每次冷 datasource open、SQL plan 与执行前重置计数 allocator，报告完整查询的
allocation calls/bytes，并同时输出 rows/s、P95、进程峰值 RSS 和 reader 输入 buffer 峰值；
allocation traffic 与 RSS 是两个独立指标。
