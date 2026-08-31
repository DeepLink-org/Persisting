# 严格 ATIF 往返

**问题：`pchronicle import/export --strict` 能否按字节恢复 ATIF？可复现结论：输入与导出经 `jq --sort-keys` 后 `cmp` 相等。**

这个示例只使用 `pchronicle import/export`，并以 `--strict` 恢复 ATIF。输入输出使用
相同的 `jq --sort-keys` 格式化后按字节比较。强制 Storyline hub 往返由
`persisting-pchronicle-cli` 的集成测试覆盖。

## Run

```bash
./run.sh
```

## Links

- [pChronicle examples](../README.md)
- [Import and export](../../../docs/src/pchronicle/guides/exchange.zh.md)
