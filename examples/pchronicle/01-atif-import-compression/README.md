# 2.1 导入 ATIF 并比较压缩比

问题：确定性 ATIF JSONL 导入 pChronicle 三表 Lance 后，实际物理体积是多少？

脚本复制仓库的 8 个 ATIF fixtures，默认生成 64 组、共 512 条 trajectory，然后调用
pChronicle typed importer。指标是原始 JSONL bytes 与完整 Lance generation bytes。

```bash
./run.sh
```

物理体积依赖 corpus；脚本通过 `wc` 和 `du` 直接打印两边的实际大小，产物保留在
`.work/` 中。
