# 2.1 导入 ATIF 并比较压缩比

问题：确定性 ATIF JSONL 导入 pChronicle 三表 Lance 后，实际物理体积是多少？

脚本以仓库的 8 个 ATIF fixtures 作为结构模板，默认生成 64 组、共 512 条 trajectory，
然后调用 `ppilot chronicle import`。message 文本不是重复 fixture 内容，而是从仓库的
Rust、Python、Shell 和 TOML 源码块中按固定随机种子抽取；这样保留可复现性，同时避免
大规模重复文本人为放大列压缩收益。可通过 `PCHRONICLE_CORPUS_SEED` 更换种子。
指标是原始 JSONL bytes 与完整 Lance store bytes。

```bash
./run.sh
```

脚本把当前仓库的 `target/release` 加入 `PATH`，随后只通过标准 `ppilot` 命令调用产品
CLI。

脚本统计 ATIF 文件和完整 Lance store（包含数据、索引及版本元数据）的精确文件字节，
并直接给出 Lance 占 ATIF 的比例、节省空间百分比和 `ATIF/Lance` 压缩倍数，同时输出
便于脚本采集的 `RESULT benchmark=storage ...`。物理体积依赖 corpus，产物保留在
`.work/` 中。
