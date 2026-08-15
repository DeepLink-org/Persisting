# 查询持久化 Run 历史

`pChronicle` 从本地目录或 S3 读取轨迹 Dataset，发现支持的 Source 格式，并提供规范化的
`runs`、`steps` 与 `tool_calls` 表。

它是执行结束后的历史层，不是 runtime 或 scheduler。

## 从已知 Dataset 开始

在 Persisting 源码目录中运行：

```bash
pchronicle ls examples/data/atif
pchronicle analysis overview examples/data/atif
```

`ls` 展示发现的 Source；overview 在无需先写 SQL 的情况下汇总 Dataset 结构。

## 提出一个具体问题

```bash
pchronicle query examples/data/atif \
  'SELECT source, COUNT(*) AS steps
   FROM dataset.steps
   GROUP BY source
   ORDER BY source'
```

查询是只读的。ATIF、ACTF、OpenAI Messages、canonical events 与 Storyline Source
会在 Dataset 边界被规范化，因此语义对齐的部分可以使用同一条查询。

## 你刚刚完成了什么

你发现了一个 Dataset 中的逻辑 Source，构造了一次 Catalog Snapshot，运行了稳定的内置
汇总，并查询了规范化关系。整个过程没有修改 Dataset 内容。

按任务继续：

- [发现并查询自己的 Dataset](guides/discover-and-query.md)。
- [使用 pVisor 捕获新 Run](../pvisor/guides/capture.md)。
- [导入或导出轨迹](guides/exchange.md)。
- [理解 Dataset identity 与 Snapshot](concepts/dataset-and-source.md)。
- 在 [`pchronicle` 参考](reference/cli.md)中查找精确参数。
