# 排查一个 Dataset

每次排查 pChronicle 结果时都按相同顺序进行：先确认路径，再查看可见内容，最后缩小查询。
这样可以区分 Dataset 缺失、结果为空和资源上限，而不是把它们看成同一个错误。

## 先确认 Dataset

排查时先使用具体路径。alias 会增加一次解析步骤：

```bash
pchronicle alias list
pchronicle status ./trajectory-data --format json
pchronicle ls ./trajectory-data --format json
```

如果 alias 失败，先解析 alias，再排查存储凭据或 SQL：

```bash
pchronicle alias get-url prod
pchronicle status @prod --format json
```

Alias 只指向 Dataset，不会复制或移动底层数据。

## Dataset 能打开但看起来为空

在编写更复杂的过滤器前先看汇总：

```bash
pchronicle analysis overview ./trajectory-data
pchronicle find ./trajectory-data --match "" --format json
```

空结果可能表示路径包含受支持格式但没有匹配记录，也可能是过滤器作用在错误的实体上，或
Dataset 中的文件不是 pChronicle 可以识别的格式。overview 和 JSON metadata 会说明可见
Source 与搜索模式。

## 查询没有返回行

先执行有上限的 count，再确认规范化表名：

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source'
```

先用 `find` 定位 identity 或文本，再编写 join。Snapshot 会固定一次读取视图；如果两次命令
之间数据发生变化，请记录 JSON 输出中的 Snapshot identifier，并在后续查询中复用。

## 查询触发资源上限

资源上限是公共查询契约的一部分。先缩小问题，再提高限制：

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) FROM dataset.steps GROUP BY source' \
  --max-output-rows 20 --timeout 10s
```

CI 中使用 `--file` 和明确的输出限制。需要更大预算时，应在调用工作流中说明原因，而不是
静默移除保护。

## Source 格式不受支持

先查看[支持的格式](../reference/formats/index.md)，再使用 exchange 指南导入为 pChronicle
可以规范化的 Dataset。导入不会补造缺失的 lineage 或 Evidence；需要追溯时，请保留原始
Source 与规范化视图。

## 提交 issue 前

请提供 pChronicle 版本、Dataset 路径或 alias 名称（不要包含凭据）、`status --format json` 输出、
完整查询和资源限制。对象存储还应说明 Provider 类型及 region 或 endpoint，但不要提供 access key
或签名 URL。
