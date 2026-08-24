# 查看轨迹 Dataset

pChronicle 使用同一套接口读取本地、对象存储或用户 alias 指向的 Agent 轨迹。除非显式运行带
目标位置的 `import` 或 `export`，命令不会修改 Dataset。

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

`ls` 显示 pChronicle 可以使用的轨迹数据；`analysis overview` 无需编写 SQL，即可给出稳定汇总。

## 3. 提出一个具体问题

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) AS steps
         FROM dataset.steps
         GROUP BY source
         ORDER BY source'
```

查询是有界、只读的。pChronicle 会在语义对齐时，把支持的轨迹格式规范化为公共的 `runs`、
`steps` 和 `tool_calls` 表。

## 你刚刚完成了什么

你打开了一个 Dataset，运行了内建汇总，并查询了规范化表。整个过程没有修改 Dataset。

按任务继续：

- [发现并查询自己的 Dataset](guides/discover-and-query.md)
- [导入或导出轨迹](guides/exchange.md)
- [使用 alias 并查阅完整命令行](reference/cli.md)
- [使用 pVisor 采集新 Run](../pvisor/guides/capture.md)
- [理解 Dataset 接口](concepts/index.md)
