# 3. 跨 Dataset SQL

**问题：一条 SQL 能否同时查询三种交换格式？可复现结论：`--mount` 把 ATIF / ACTF / OpenAI Messages 挂成独立 schema，Run 计数为 1 / 1 / 2，并返回四条统一 `session_id`。**

这个示例用重复的 `--mount NAME=DATASET` 参数同时挂载三种交换格式：

- `atif=examples/data/atif`
- `actf=examples/data/actf`
- `openai=examples/data/openai-messages`

每个挂载名都会成为独立 SQL schema，因此可以在同一条语句中访问 `atif.runs`、
`actf.runs` 和 `openai.runs`。脚本先用标量子查询比较三个 Dataset 的 Run 数量，再用
`UNION ALL` 展示不同输入格式映射出的统一 `session_id`。

## Run

```bash
./run.sh
```

脚本验证两条 SQL 语句的完整结果集；它只读取仓库内 fixture，不创建持久 Dataset。
完整命令日志保存在本场景的 `.work/run.*`。

## Links

- [pChronicle examples](../README.md)
- [Query model](../../../docs/src/pchronicle/reference/query-model.zh.md)
