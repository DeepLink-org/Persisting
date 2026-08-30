# 直接查询 OpenAI Messages 与 ACTF Dataset

**问题：交换格式目录能否不经 import 就映射为统一 SQL 表？可复现结论：OpenAI Messages 得到 2 个 session / 4 个 Step；ACTF 得到 `example-code-repair` → `actf-agent`。**

`pchronicle query` 会发现目录中的交换格式文件，并把它们投影为统一的 `dataset.runs`
和 `dataset.steps` 表。示例无需预先转换存储格式。

## Run

```bash
./run.sh
```

## Links

- [pChronicle examples](../README.md)
- [Discover and query](../../../docs/src/pchronicle/guides/discover-and-query.zh.md)
