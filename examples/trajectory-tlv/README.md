# Session markdown 示例

[`0001.md`](demo-agent/demo-run-001/0001.md)：每块为 `<!-- persisting:block:{speaker} {json} -->` + **裸 Markdown 正文**（预览即对话；compact 可还原为 `CaptureRecord` 并写入 Vortex）。

```bash
./target/debug/persisting trajectory replay \
  ./examples/trajectory-tlv --agent-id demo-agent --session-id demo-run-001 \
  --storage markdown --input examples/trajectory-tlv/demo-agent/demo-run-001/0001.md
```
