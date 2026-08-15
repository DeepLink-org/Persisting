# 发现并查询 Dataset

当你已经有本地目录或 S3 prefix，需要先理解内容再分析时，使用这个工作流。

## 1. 发现逻辑 Source

```bash
pchronicle ls ./dataset
pchronicle status ./dataset
```

`ls` 报告逻辑 Source；`status` 汇总本次命令选择的不可变 Catalog Snapshot。自动化中使用
JSON 输出：

```bash
pchronicle ls ./dataset --format json
```

Dataset 可能包含损坏 Source 时，显式选择错误策略：

```bash
pchronicle ls ./dataset --errors report
pchronicle ls ./dataset --errors strict
```

## 2. 从稳定分析开始

```bash
pchronicle analysis overview ./dataset
pchronicle analysis agents ./dataset
pchronicle analysis models ./dataset
pchronicle analysis tools ./dataset
```

内置 analysis 适合常见汇总；需要自定义 projection 时再进入 SQL。

## 3. 检查逻辑 Schema

不要假设交换格式中的物理字段就是 SQL column：

```bash
pchronicle query ./dataset "DESCRIBE dataset.steps"
```

常见逻辑关系包括 `sources`、`runs`、`steps`、`tool_calls`、`events` 和
`trajectories`，实际可用关系由每个 Source 报告。

## 4. 提出有界问题

```bash
pchronicle query ./dataset \
  "SELECT session_id, COUNT(*) AS steps
   FROM dataset.steps
   GROUP BY session_id
   ORDER BY steps DESC"
```

数据管道使用 `--format jsonl|csv` 和 `--output`。Query 只读，并受行数、字节、发现规模与
timeout 上限约束。

## 5. 消除外部 ID 歧义

ID 是 Source-local。先发现候选，再在持久引用中保留 `source_path`：

```bash
pchronicle find ./dataset --session-id session-42
pchronicle find ./dataset --source nested/source.json \
  --session-id session-42
```

精确参数见 [`pchronicle` 参考](../reference/cli.md)；Source 与 Snapshot 的设计原因见
[Dataset、Source 与 Snapshot](../concepts/dataset-and-source.md)。
