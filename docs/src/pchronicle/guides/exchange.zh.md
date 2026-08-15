# 导入与导出轨迹

Import 和 export 位于互操作边界。Import 创建新 Dataset；export 从一个 Catalog Snapshot
重建完整轨迹。

## 导入到新 Dataset

```bash
pchronicle import --from input.json \
  --output ./imported --format atif
```

目标是 create-only。已有目标会被拒绝，而不是静默 append 或 replace。普通文件可以自动
识别；stdin 必须是有限且格式明确的输入：

```bash
cat input.json | pchronicle import --from - --stream \
  --output ./imported --format storyline
```

导入后检查新边界：

```bash
pchronicle status ./imported
pchronicle analysis overview ./imported
```

## 导出完整轨迹

```bash
pchronicle export --from ./imported \
  --output restored.json --format atif
```

需要时使用 Source-local identity 缩小导出范围：

```bash
pchronicle export --from ./imported --output one.json --format actf \
  --source source.json --session-id session-42 --strict
```

目标格式无法保留原交换文档时，`--strict` 会失败。输出文件默认 create-only，覆盖必须显式
请求。

Import/export 不是存储迁移协议，任意 SQL row 也不是可导出的完整轨迹。格式契约见
[轨迹格式](../reference/formats/index.md)，层次边界见
[事实、Projection 与 Revision](../concepts/facts-and-projections.md)。
