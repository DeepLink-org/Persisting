# 2.1 导入 ATIF 并比较压缩比

问题：确定性 ATIF JSONL 导入 pChronicle 三表 Lance 后，实际物理体积是多少？

脚本复制仓库的 8 个 ATIF fixtures，默认生成 64 组、共 512 条 trajectory，然后调用
pChronicle typed importer。指标是原始 JSONL bytes 与完整 Lance generation bytes。

```bash
./run.sh
REPLICAS=128 ./run.sh
```

压缩比依赖 corpus；脚本输出实际比率，并只在 Lance 小于该固定 ATIF 输入时通过。
