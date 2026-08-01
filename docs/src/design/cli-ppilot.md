# `ppilot` — Run 编排与轨迹分析 CLI

统一入口是 `persisting batch` 与 `persisting query`。`ppilot` 仍作为组件级入口保留，
用于独立部署、调试和自检。pPilot 负责批量 Run 编排与查询交互；轨迹格式、
Lance/ATIF datasource 与 SQL 执行继续由 pChronicle library 提供。

## 命令

```text
ppilot run <SCRIPT> [OPTIONS]
ppilot query <INPUT> (--sql <SQL> | --sql-file <FILE|->) [--source auto|lance|atif]
ppilot self-test [OPTIONS]
```

前两条命令分别等价于：

```bash
persisting batch <SCRIPT> [OPTIONS]
persisting query <INPUT> (--sql <SQL> | --sql-file <FILE|->) [--source auto|lance|atif]
```

### `run`

运行 `plan()` + `execute(item)` workload，提供有界并发、基础设施重试、checkpoint、
resume、结果 sink 和可选 trajectory sink。已有 pPilot 参数全部位于这个子命令下：

```bash
ppilot run plan.py --workers 8 --per-worker 2 --sink ./results --resume
```

### `query`

对 Storyline 三表 Lance store 或 ATIF 输入执行一条只读 DataFusion SQL。两个后端暴露
相同的 `runs`、`steps`、`tool_calls` 表，默认按 `<INPUT>/CURRENT` 是否存在自动识别；
可通过 `--source` 覆盖。结果只向 stdout 输出 JSONL，错误只向 stderr 输出。

```bash
ppilot query ./storyline-store \
  --sql "SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source"

ppilot query ./trajectories.ndjson --source atif --sql-file analysis.sql
cat analysis.sql | ppilot query ./storyline-store --sql-file -
```

只允许单条 `SELECT`、`VALUES`、`DESCRIBE` 或 `EXPLAIN`；拒绝 DDL、DML、`COPY` 和
多语句。未显式 `ORDER BY` 时不保证结果顺序。

### `self-test`

运行内置三任务 workload，检查 Python、plan、execute 和本地 worker 链路：

```bash
ppilot self-test --python python3 --workers 2
```

## 构建边界

公共 binary 使用 `cli` feature，它会启用 pChronicle/Lance/DataFusion；默认 pPilot
library 构建不启用该 feature，嵌入式调度器不会承担查询依赖：

```bash
cargo build -p persisting-ppilot --features cli --bin ppilot
```
