# Session markdown 示例

[`demo-run-001.md`](demo-agent/demo-run-001/demo-run-001.md)：每块为 `<!-- persisting:block:{speaker} {json} -->` + Markdown 正文，可直接预览或由 pChronicle 读取。

```bash
./target/debug/persisting trajectory replay \
  ./examples/trajectory-agenticmd/demo-agent/demo-run-001 \
  --storage-format markdown
```
