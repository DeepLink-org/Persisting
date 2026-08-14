# 严格 ATIF 往返

这个示例只使用 `pchronicle import/export`，并以 `--strict` 恢复 ATIF。输入输出使用
相同的 `jq --sort-keys` 格式化后按字节比较。强制 Storyline hub 往返由
`persisting-pchronicle-cli` 的集成测试覆盖。

```bash
./run.sh
```
