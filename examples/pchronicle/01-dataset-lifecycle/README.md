# 1. Dataset 生命周期

**问题：一份 ATIF 文件能否走完 import → inspect → query → find → 严格 export？可复现结论：隔离 Warehouse 得到 ready Dataset（1 run / 3 steps / 1 tool call），且 `--strict` ATIF 导出与规范化输入按字节一致。**

这个示例从一份 ATIF 文件创建隔离的本地 Warehouse 和 Dataset，然后依次执行：

1. `pchronicle import` 导入轨迹；
2. `ls --physical` 与 `status` 检查 Source 和统计信息；
3. `query` 聚合 Steps；
4. `find` 定位指定 Session 中的 Step；
5. `export --strict` 无损导出并验证 JSON 数据模型。

## Run

```bash
./run.sh
```

脚本使用 [`examples/data/atif/support-ticket.json`](../../data/atif/support-ticket.json)
作为确定性输入，不会读写用户的默认设置。每次运行会在 `.work/run.*` 中创建独立的
settings、Warehouse 和输出文件，并在结束时打印产物路径。

## Links

- [pChronicle examples](../README.md)
- [Import and export](../../../docs/src/pchronicle/guides/exchange.zh.md)
