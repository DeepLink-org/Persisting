# 导入与导出轨迹

Import 和 export 位于互操作边界。Import 创建新 Dataset；export 从已有 Dataset 读取完整轨迹。
Import 与 export 均接受 ATIF、ACTF、OpenAI Messages 和 Storyline JSON。
Import 还接受仅解码的 Codex（`codex`）与 Claude Code（`claude-code`）会话 JSONL；
export 拒绝这两种格式。

## 导入到新 Dataset

```bash
pchronicle import --from input.json \
  --to ./imported --input-format atif
```

目标是 create-only。已有目标会被拒绝，而不是静默 append 或 replace。普通文件可以自动
识别。目录输入会递归扫描 `.json`、`.jsonl` 与 `.ndjson` 文件；默认输出会保留其相对
路径。未指定 `--input-format` 时按文件分别探测类型；无法识别为轨迹格式的 JSON 会跳过并警告：

```bash
pchronicle import --from ./corpus --to ./imported
pchronicle import --from ./codex-sessions --to ./codex-ds --input-format codex
pchronicle import --from ./claude-sessions --to ./claude-ds --input-format claude-code
```

默认输出逐字节保留输入文件。若要把所有解码后的输入规范化并 squash 成输出根目录下的
一个 Storyline Lance Store：

```bash
pchronicle import --from ./corpus --to ./normalized \
  --output-format storyline
```

经过验证且非空的 canonical Event Store 会在 JSON 扫描前被识别，并始终创建
Storyline Lance：

```bash
pchronicle import --from ./run/events.lance --to ./run/storyline
```

此模式接受本地和 object-store URI，不修改源，并拒绝已有目标。JSON 结果报告
`format: "events"`、`output_format: "storyline-lance"` 和 `fact_rows`，不包含
`input_bytes`。canonical events 不接受显式 `--output-format preserve` 或 JSON exchange
`--input-format`。

squash 后，Dataset 所有规范化表中的 `_file_` 都是 `.`：

```bash
pchronicle query ./normalized \
  --sql 'SELECT _file_, COUNT(*) AS runs FROM dataset.runs GROUP BY _file_'
```

所有输入的 `document_id` 与 `session_id` 必须分别全局唯一；冲突会使整次导入失败，并报告
两个原始路径。成功的 Storyline Store 不把这些路径保存为可查询 provenance。需要保留
文件边界时应使用默认的 preserve 输出。

ATIF `.jsonl` 与 `.ndjson` 输入会逐条解码其中的非空记录。递归扫描目录时会跳过软链接；
若 `--from` 显式指定一个指向普通文件的软链接，则仍按单文件导入。只有所有输入和所选
物理输出都成功后才会原子发布完整输出目录。单文件 preserve 导入会使用
`trajectories.atif.jsonl`/`trajectories.atif.ndjson`，确保后续查询仍按行式容器读取。stdin 必须
是有限且格式明确的输入：

```bash
cat input.json | pchronicle import --from - \
  --to ./imported --input-format openai-messages
```

导入后检查新边界：

```bash
pchronicle status ./imported
pchronicle analysis overview ./imported
```

## 导出完整轨迹

```bash
pchronicle export --from ./imported \
  --to restored.json --output-format atif
```

需要时使用文件路径与外部 ID 缩小导出范围：

```bash
pchronicle export --from ./imported --to one.json --output-format actf \
  --source source.json --session-id session-42 --strict
```

目标格式无法保留原交换文档时，`--strict` 会失败。输出文件默认 create-only，覆盖必须显式
请求。

Import/export 不是存储迁移协议，任意 SQL row 也不是可导出的完整轨迹。格式契约见
[轨迹格式](../reference/formats/index.md)，层次边界见
[数据契约与 Revision](../concepts/facts-and-projections.md)。
