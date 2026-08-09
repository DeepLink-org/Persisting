# 2.7 `objects.lance` 大对象拆分收益

这个示例回答一个具体问题：同一批轨迹中的大文本直接留在三表 JSON 列，和按内容寻址
拆到共享 `objects.lance`，在物理体积和分析性能上分别有什么差异？

脚本生成确定性的 ATIF NDJSON。默认包含 512 个 step，每个 step 在四个独立内容列引用
同一个 32 KiB 文本，而全部轨迹只使用 8 个唯一 blob。这个数据集刻意模拟 system prompt、
工具输出、代码快照或图像等内容在多轨迹、多列间高度复用的情况；它不是一般生产数据的
压缩率预测。Lance 对单列内部的重复值本身也能压缩，`objects.lance` 的额外价值是跨列、
跨轨迹共享同一个内容寻址对象，并让三表扫描不必携带大内容。

运行：

```bash
./run.sh
```

脚本建立两个 schema 完全相同的 Storyline store：

- `inline` 把 offload threshold 设为 1 GB，强制内容留在三表；
- `offloaded` 把 threshold 设为 4 KiB，使用 BLAKE3 内容地址、zstd 压缩和 256-byte preview。

随后按精确文件字节统计完整 store、`objects.lance` 与其余三表/control 体积，并报告：

- 逻辑引用字节与唯一内容字节，用于解释跨引用去重倍率；
- offloaded/inline 物理体积、节省比例、唯一内容/`objects.lance` 压缩率，以及包含去重的
  逻辑内容/完整 store 有效压缩率；
- 元数据聚合在两个 store 上的 median、P95、rows/s 和峰值 RSS；
- 完整内容聚合在两个 store 上的同一组指标，并先验证查询结果逐字节一致；
- offloaded store 的 preview 查询，量化只返回 descriptor 内嵌的用户内容头部、不读取完整
  blob 的路径；内部 descriptor 本身不会出现在查询结果中。

性能计时包含独立 CLI 进程启动、store open、SQL 规划和执行；每轮轮换五条查询的顺序。
元数据查询和完整内容查询分别做严格语义等价校验。preview 只返回描述符中内嵌的头部，
语义有意不同，因此只与 offloaded 完整展开路径比较成本，不冒充同一查询的加速比。

完整展开会进行对象查找、读取、解压以及 hash/length 校验，通常比 inline 读取更贵；这正是
拆分设计的边界，而不是被隐藏的成本。收益主要来自高复用数据的跨列去重，以及大量只读
结构化字段或只需预览内容的分析任务。实际收益由 blob 大小、唯一率、列间复用、查询投影、
存储介质和缓存命中率共同决定。

可通过环境变量调整规模和复用度：

```bash
PCHRONICLE_BLOB_TRAJECTORIES=256 \
PCHRONICLE_BLOB_STEPS=16 \
PCHRONICLE_BLOB_UNIQUE=32 \
PCHRONICLE_BLOB_BYTES=65536 \
PCHRONICLE_BLOB_BENCH_ITERS=20 ./run.sh
```

最后一行 `RESULT benchmark=objects_lance_blob_offload ...` 便于 CI 或外部基准收集器解析。
