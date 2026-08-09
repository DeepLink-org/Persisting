# 2.2 raw Python JSON 与 pChronicle 查询性能

问题：相对最直接的 Python JSON 解析方式，pChronicle 直接查询 JSON 和查询 Lance 分别
有什么成本与收益？

统一基线用 Python 标准库打开完整 ATIF JSONL、逐行 `json.loads`，再以手写循环完成等价
过滤或聚合。两条 pChronicle 路径分别是：

- `pChronicle JSON`：`ppilot query` 直接查询 ATIF 文件；
- `pChronicle Lance`：先 `ppilot chronicle import`，再查询复用的 Lance store。

三条路径都由独立进程执行，包含进程启动以及各自的 parse/open/query 成本。每轮轮换执行
顺序，先验证输出语义相等，再报告 median 和 p95。所有相对值都严格定义为
`Python median / measured-path median`；大于 1 表示 pChronicle 更快，小于 1 表示 Python
基线更快。导入时间和存储体积单独报告，不计入单次查询 speedup。

和 01 相同，trajectory 结构来自 ATIF fixtures，但 message 内容按固定种子从仓库源码
块中抽取，避免重复 fixture 文本扭曲存储与扫描结果。

默认使用 64 组数据和 20 次冷 CLI 查询：

```bash
./run.sh
```

可以直接用环境变量做规模对照：

```bash
PCHRONICLE_BENCH_SCALE=256 PCHRONICLE_BENCH_ITERS=10 ./run.sh
```

脚本通过 `PATH` 使用当前仓库构建的 pPilot，并让 `ppilot query` 根据输入自动识别 ATIF
文件和 Lance store。Python 基线只代表这两条固定查询的最小手写实现，不具备 SQL、格式
自动识别、资源限制和统一 schema 等 pChronicle 能力。这里测的是一次命令一次查询的冷
进程延迟，不代表常驻进程吞吐。示例固定使用 release 版本；不同机器上的数字不能直接
横向比较。
