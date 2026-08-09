# 2.1 raw JSON 与 pChronicle Lance 存储体积

问题：相对未经额外处理的 ATIF JSONL，导入 pChronicle Lance 后的完整物理体积是多少？

脚本以仓库的 8 个 ATIF fixtures 作为结构模板，默认生成 64 组、共 512 条 trajectory，
然后调用 `ppilot chronicle import`。message 文本不是重复 fixture 内容，而是从仓库的
Rust、Python、Shell 和 TOML 源码块中按固定随机种子抽取；这样保留可复现性，同时避免
大规模重复文本人为放大列压缩收益。可通过 `PCHRONICLE_CORPUS_SEED` 更换种子。
存储基线是 raw ATIF JSONL 的文件字节数；增强路径是完整 pChronicle Lance store 的文件
字节数。pChronicle 直接 JSON 查询复用原文件，因此不产生另一份存储体积。

```bash
./run.sh
```

脚本把当前仓库的 `target/release` 加入 `PATH`，随后只通过标准 `ppilot` 命令调用产品
CLI。

脚本统计 raw JSON 和完整 Lance store（包含数据、索引及版本元数据）的精确文件字节，
并直接给出 Lance 占 raw JSON 的比例、节省空间百分比和 `JSON/Lance` 比值，同时输出
便于脚本采集的 `RESULT benchmark=storage ...`。物理体积依赖 corpus，产物保留在
`.work/` 中。
