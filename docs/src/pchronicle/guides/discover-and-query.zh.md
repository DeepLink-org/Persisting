# 发现并查询 Dataset

当你已有本地路径、对象存储 URI 或 alias，希望先理解其中的轨迹数据再编写报告时，使用这个
工作流。

## 1. 检查 Dataset

```bash
pchronicle ls ./dataset
pchronicle status ./dataset
```

`ls` 显示 pChronicle 发现的、可以独立查询的轨迹数据项；`status` 汇总 Dataset 是否可用以及
包含哪些数据。自动化中使用 JSON 输出：

```bash
pchronicle ls ./dataset --format json
```

Dataset 可能包含损坏条目时，显式选择错误策略：

```bash
pchronicle ls ./dataset --errors report
pchronicle ls ./dataset --errors strict
```

## 2. 从内建分析开始

```bash
pchronicle analysis overview ./dataset
pchronicle analysis agents ./dataset
pchronicle analysis models ./dataset
pchronicle analysis tools ./dataset
```

内建 analysis 覆盖常见汇总；需要自定义筛选、join 或聚合时再进入 SQL。

## 3. 检查查询 Schema

```bash
pchronicle query ./dataset --sql "DESCRIBE dataset.steps"
```

常见关系包括 `sources`、`runs`、`steps`、`tool_calls`、`events` 和 `trajectories`；实际可用
关系取决于 Dataset 内容。

## 4. 提出有界问题

```bash
pchronicle query ./dataset \
  --sql "SELECT session_id, COUNT(*) AS steps
         FROM dataset.steps
         GROUP BY session_id
         ORDER BY steps DESC"
```

数据管道使用 `--format jsonl|csv` 和 `--output`。Query 只读，并受行数、字节、发现规模与
timeout 上限约束。

## 5. 消除重复外部 ID 的歧义

同一个外部 ID 可能出现在多个文件中。先查找候选，需要持久引用时再保留 `source_path`：

```bash
pchronicle find ./dataset --session-id session-42
pchronicle find ./dataset --source nested/source.json \
  --session-id session-42
```

精确参数见 [`pchronicle` 命令行参考](../reference/cli.md)，字段和 join 规则见
[查询模型](../reference/query-model.md)。内部发现与版本固定机制属于
[Dataset Catalog 设计](../design/catalog.md)。
