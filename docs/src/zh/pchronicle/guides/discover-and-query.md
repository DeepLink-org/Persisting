# 发现并查询 Dataset

当你已有本地路径、对象存储 URI 或 alias，希望先理解其中的 Run 数据再编写报告时，使用这个
工作流。

:::tip 完成后你会得到什么
你会知道 Dataset 包含哪些 Source、有哪些可用 relation，以及如何可复现地回答一个受资源限制的只读问题。
:::

## 1. 检查 Dataset

```bash
pchronicle ls ./dataset
pchronicle status ./dataset
```

`ls` 显示 pChronicle 发现的、可以独立查询的 Run 数据源；`status` 汇总 Dataset 是否可用以及
包含哪些数据。自动化中使用 JSON 输出：

```bash
pchronicle ls ./dataset --format json
```

Dataset 可能包含损坏条目时，显式选择错误策略：

```bash
pchronicle ls ./dataset --errors report
pchronicle ls ./dataset --errors strict
```

探索陌生数据时使用 `report`。在自动化任务中，如果不完整 Dataset 应该让任务失败而不是产生部分结果，再切换到 `strict`。

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

## 4. 提出受资源上限保护的问题

```bash
pchronicle query ./dataset \
  --sql "SELECT session_id, COUNT(*) AS steps
         FROM dataset.steps
         GROUP BY session_id
         ORDER BY steps DESC"
```

数据管道使用 `--format jsonl|csv` 和 `--output`。Query 只读，并受行数、字节、发现规模与
timeout 上限约束。

## 5. 先定位，再分析

`ls` / `sources` 负责发现；`find` 在已 pin 的 Snapshot 内定位；`query` 负责分析。
CLI `--match` 与 Web `q` 共用同一表达式、报告的 scope 和 `snapshot_id`；Web UI 可以对
返回字段做高亮，不改变命中集合。

```bash
pchronicle find ./dataset --match "timeout" --format json
```

用返回的 `source_path`、session 和 step 身份去收窄 SQL。

## 6. 消除重复外部 ID 的歧义

同一个外部 ID 可能出现在多个文件中。先查找候选，需要持久引用时再保留 `source_path`：

```bash
pchronicle find ./dataset --session-id session-42
pchronicle find ./dataset --source nested/source.json \
  --session-id session-42
```

精确参数见 [`pchronicle` 命令行参考](../reference/cli.md)，字段和 join 规则见
[查询模型](../reference/query-model.md)。内部发现与版本固定机制属于
[Snapshot 设计](../design/catalog.md)。
