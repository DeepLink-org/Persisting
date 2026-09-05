# 查看 Run Dataset

pChronicle 使用同一套接口读取本地、对象存储或用户 alias 指向的 Agent 运行记录。本页中的
浏览、find、analysis 和 query 命令都是只读的。

## 1. 不准备数据，直接体验

```bash
pchronicle onboard
```

Walkthrough 会创建临时示例 Dataset，并依次介绍主要命令。也可以直接跳到查询：

```bash
pchronicle onboard query
```

两条命令都不要求源码 checkout，也不要求已有 Dataset。

## 2. 浏览自己的 Dataset

Dataset 可以是本地路径、对象存储 URI 前缀，或 `@prod` 这样的用户 alias：

```bash
pchronicle ls ./trajectory-data
pchronicle analysis overview ./trajectory-data
```

`ls` 显示 pChronicle 可以使用的 Run 数据；`analysis overview` 无需编写 SQL，即可给出稳定汇总。

需要定位具体内容时，使用统一的 `find --match` 语法：

```bash
pchronicle find ./trajectory-data --match "timeout" --format json
pchronicle find ./trajectory-data --match '#system("retry")'
```

## 3. 提出一个具体问题

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) AS steps
         FROM dataset.steps
         GROUP BY source
         ORDER BY source'
```

查询只读，并受明确的资源上限约束。pChronicle 会在语义对齐时，把支持的运行数据格式规范化为公共的 `runs`、
`steps` 和 `tool_calls` 表。

## 你刚刚完成了什么

你打开了一个 Dataset，运行了内建汇总，查询了规范化表，并可以用 FTS/JSONB 定位具体轨迹。整个过程没有修改 Dataset。

按任务继续：

- [发现并查询自己的 Dataset](guides/discover-and-query.md)
- [导入或导出 Run](guides/exchange.md)
- [查看统一产品术语](reference/terminology.md)
- [使用 alias 并查阅完整命令行](reference/cli.md)
- [使用 pVisor 采集新 Run](../pvisor/guides/capture.md)
- [理解 pChronicle 核心概念](concepts/index.md)
