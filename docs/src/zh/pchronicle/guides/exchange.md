# 导入与导出 Run

Import 和 export 位于互操作边界。Import 可以创建、追加或替换 Dataset；export 从已有 Dataset 读取完整 Run。
Import 与 export 均接受 ATIF、ACTF、OpenAI Messages、Storyline JSON 和记录级 Compact JSONL。
Import 还接受仅解码的 Codex（`codex`）与 Claude Code（`claude-code`）会话 JSONL；
export 拒绝这两种格式。

Compact JSONL 每行保留一个 JSON object，不赋予轨迹语义。使用
`--input-format compact-jsonl` 或 `--output-format compact-jsonl`；`--column` 映射与 snapshot sync
限制见[命令参考](../reference/cli.md)。缺少可用 `id` 的记录会生成稳定的
`source_filename#line_number`；export 按原始输入字节保留记录。compact import 成功后还会在
dataset 根写入 leaf `chronicle.manifest`，便于后续 discovery 不必仅为分类打开 Lance
（[RFC-0015](../../rfcs/0015-chronicle-manifest.md)）。

## 导入到新 Dataset

```bash
pchronicle import --from input.json \
 --to ./imported --input-format atif
```

默认会拒绝已有目标。`--append` 用于已有 Storyline Dataset；重复
`document_id` 默认增加 `#N` 后缀，也可用 `--on-duplicate skip` 跳过。`--replace` 会先
把完整导入写入临时路径，确认后以 rename 事务替换已有的本地 Dataset，最后才删除旧数据；要求
交互确认或传入 `--yes`。对象存储 Dataset 的 replace 会先清空目标前缀再写入（非原子；中断可能导致目标暂时为空）。普通文件可以自动识别。目录输入会递归扫描
`.json`、`.jsonl` 与 `.ndjson` 文件；默认输出会保留其相对
路径。未指定 `--input-format` 时按文件分别探测类型；无法识别为运行数据格式的 JSON 会跳过并警告：

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

## 导出完整 Run

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

Import/export 不是存储迁移协议，任意 SQL row 也不是可导出的完整 Run。精确参数见
[`pchronicle` 命令参考](../reference/cli.md)。格式契约见
[运行数据格式](../reference/formats/index.md)，层次边界见
[数据契约与 Revision](../concepts/facts-and-projections.md)。
