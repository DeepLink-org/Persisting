# `ppilot` — Run 编排与轨迹分析 CLI

统一入口是 `persisting batch` 与 `persisting query`。`ppilot` 仍作为组件级入口保留，
用于独立部署、调试和自检。pPilot 负责批量 Run 编排与查询交互；轨迹格式、
Lance/ATIF datasource 与 SQL 执行继续由 pChronicle library 提供。

## 命令

```text
ppilot run <SCRIPT> [OPTIONS]
ppilot chronicle import <INPUT> <STORE>
ppilot query sql <INPUT> (--sql <SQL> | --sql-file <FILE|->) [--source auto|lance|atif] [--table NAME=FORMAT:PATH]...
ppilot query point <STORE> --session-id <ID> [--step-id <N>]
ppilot query batch <STORE> --session-id <ID[,ID]...> [--step-id <N>]
ppilot query follow <STORAGE> --agent-id <ID> --session-id <ID> [--offset <N>] [--limit <N>] [--poll-interval-ms <MS>]
ppilot produce <PLANNER.py> --output <DIR> [--parallelism N] [-- <PLANNER_ARGS>...]
ppilot analysis <INPUT> [--output <DIR>] [--fmt jsonl|json|toml] [--parallelism N] (--sql <SQL> | --sql-file <FILE>)
ppilot process <INPUT> (--script <FILE> | --count <METRIC>) [--mappers N] [--output <DIR>]
ppilot self-test [OPTIONS]
```

`run` 和 `query` 分别等价于：

```bash
persisting batch <SCRIPT> [OPTIONS]
persisting query <INPUT> (--sql <SQL> | --sql-file <FILE|->) [--source auto|lance|atif] [--table NAME=FORMAT:PATH]...
```

### `run`

运行 `plan()` + `execute(item)` workload，提供有界并发、基础设施重试、lease fencing、
启动 reconcile、RunCommit CAS、checkpoint/resume、结果 sink 和可选 trajectory sink。
已有 pPilot 参数全部位于这个子命令下：

```bash
ppilot run plan.py --workers 8 --per-worker 2 --sink ./results --resume

# 将 canonical Run control 放在 S3，本地 sink 保留可重放的完整结果
ppilot run plan.py --sink ./results \
  --control-uri s3://my-bucket/project/control --lease-ttl-ms 30000
```

`--sink` 会自动启用 durable result journal、lease epoch 和 reconciler；
`--control-uri` 可覆盖默认的 sink 本地 control root，支持 pChronicle 的对象存储 URI。
它不接受凭证参数，认证沿用对象存储 provider chain。

### `chronicle import`

把 ATIF JSON 对象、数组、JSONL/NDJSON 文件或目录校验并规范化为 Storyline，然后按
`session_id` 原子写入三表 Lance store。已存在的 session 被替换，其他 session 保留。
`STORE` 可以是本地路径或 pChronicle 支持的对象存储 URI。

```bash
ppilot chronicle import ./trajectories.ndjson ./storyline-store
ppilot chronicle import ./atif-directory s3://trajectory-bucket/storylines
```

### `query`

`query` 统一承载四种 pChronicle 查询模式：`sql` 用于自由分析，`point` 查询一个 step
或完整 Storyline，`batch` 在同一快照批量查询，`follow` 持续读取运行中已经提交的
canonical event。结果只向 stdout 输出 JSONL，错误和进度只向 stderr 输出。

```bash
ppilot query point ./storyline-store --session-id run-001 --step-id 7
ppilot query point ./storyline-store --session-id run-001
ppilot query batch ./storyline-store --session-id run-001,run-002 --step-id 7
ppilot query follow ./capture --agent-id agent-001 --session-id run-001

ppilot query sql ./storyline-store \
  --sql "SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source"

ppilot query sql s3://trajectory-bucket/persisting/storylines \
  --sql "SELECT COUNT(*) AS runs FROM runs"

ppilot query sql ./trajectories.ndjson --sql-file analysis.sql
cat analysis.sql | ppilot query sql ./storyline-store --sql-file -

# 将带表头的 CSV 和 JSON 对象数组注册为外部表并联查
ppilot query sql ./storyline-store \
  --table labels=csv:./labels.csv \
  --table metadata=json:./metadata.json \
  --sql "SELECT r.session_id, l.score, m.category
         FROM runs r
         JOIN labels l USING (session_id)
         JOIN metadata m USING (session_id)"
```

原有 `ppilot query <INPUT> --sql ...` 仍作为 SQL 兼容语法保留。`batch` 不会循环执行
N 次点查：step 批查生成一次 `IN` 查询，完整轨迹批查则对三张表各读取一次同一 generation
快照，并按输入 session 顺序输出每条 Storyline。

`--table` 可重复。支持 `csv`、`json`、`jsonl`（别名 `ndjson`）；`csv` 默认把首行
作为列名，`json` 表示一个 JSON 对象数组，`jsonl`/`ndjson` 表示每行一个 JSON 对象。
表名必须匹配 `[A-Za-z_][A-Za-z0-9_]*`，且不能覆盖内建表或先前注册的外部表。

只允许单条 `SELECT`、`VALUES`、`DESCRIBE` 或 `EXPLAIN`；拒绝 DDL、DML、`COPY` 和
多语句。未显式 `ORDER BY` 时不保证结果顺序。

`s3://`、`az://` 和 `gs://` 输入会自动识别为 Lance，不需要 `--source lance`。
S3 认证使用 AWS 标准凭证链；CLI 不接受 secret 参数，也不会把凭证写入 report。

### `self-test`

运行内置三任务 workload，检查 Python、plan、execute 和本地 worker 链路：

```bash
ppilot self-test --python python3 --workers 2
```

### `produce`

执行 Python planner 的 `plan()`，把同步或异步迭代器逐项产出的完整 Run 描述，以
`--parallelism` 为上限流式创建独立 pVisor Run。planner 在独立 Python 进程运行，
`--python` 选择解释器，`--` 后参数原样进入 planner 的 `sys.argv`。pPilot 对输入施加
有界反压，不要求先构造完整批次。

每项必须包含路径安全且唯一的 `id` 以及非空 `command` 数组；可选 `agent`、`cwd` 和
字符串 `env`。默认启用捕获 Gateway；每个 Run 写入独立 workspace 和 Run Bundle，
并记录批次父 Run、task id 与 `ppilot.*` 编排字段。`--batch-id` 默认取 planner 文件名，
批次根目录写 `production-report.json`。已有 Run workspace 会拒绝覆盖。

```python
# production.py
import argparse

parser = argparse.ArgumentParser()
parser.add_argument("--count", type=int, default=10)
args = parser.parse_args()

def plan():
    for i in range(args.count):
        yield {
            "id": f"task-{i:05d}",
            "agent": "codex",
            "command": ["codex", "exec", f"Solve task {i}"],
            "env": {"TASK_INDEX": str(i)},
        }
```

```bash
ppilot produce production.py --output ./runs --parallelism 16 \
  --batch-id nightly -- --count 1000
```

版本 1 的 `.json` manifest 暂时保留为兼容输入，但不再是主接口。

### `analysis`

由 pChronicle 读取并校验 ATIF JSON/数组/JSONL/目录；pPilot 按有效 session id 排序，
根据并行数自动建立均衡、确定性、互不重叠的数据分片。每个分片运行同一条只读 SQL，
输出 `part-*.jsonl`，然后按 shard id 拼接成 `results.jsonl`，并写
`analysis-report.json`。

```bash
ppilot analysis ./atif --fmt json \
  --sql 'SELECT session_id, COUNT(*) AS steps FROM steps GROUP BY session_id'

ppilot analysis ./atif --output ./analysis --parallelism 8 \
  --sql 'SELECT session_id, COUNT(*) AS steps FROM steps GROUP BY session_id'
```

不指定 `--output` 时只把合并后的结果写到 stdout，默认格式为 JSONL；`--fmt` 可选择
JSONL、JSON 数组或 TOML。指定输出目录后才持久化 `part-*.jsonl`、
`results.<fmt>` 和 `analysis-report.json`，stdout 保持为空。TOML 本身没有 null，查询
结果包含 null 时会明确报错，可改用 JSON/JSONL。

SQL 是逐分片执行的。全局聚合会产生每个分片一条 partial aggregate；需要单一全局
结果时使用不分片的 `ppilot query`，或在下游对 partial rows 做 reduce。

### `process`

`--script` 启用 Python map/reduce。driver 读取脚本和 ATIF 输入，按 `--mappers` 建立
确定性分片，并把脚本字节和数据一起发送给 Pulsing worker；远端节点不需要共享输入
目录或预装脚本文件。脚本定义 `map(records, context)` 与 `reduce(partials, context)`；也
接受 `mapper`/`reducer` 名称以及省略 `context` 的单参数函数。返回值必须能编码为
JSON。脚本 stdout 会重定向到 stderr，避免污染协议输出。

```python
def map(records, context):
    return {"runs": len(records),
            "steps": sum(len(record["steps"]) for record in records)}

def reduce(partials, context):
    return {"runs": sum(p["runs"] for p in partials),
            "steps": sum(p["steps"] for p in partials)}
```

```bash
ppilot process ./atif --script metrics.py --mappers 8
ppilot process ./atif --script metrics.py --mappers 8 --output ./processed
```

未指定 `--output` 时最终 reduce 值写到 stdout；指定后写入 `results.json` 和
`process-report.json`。报告记录脚本 SHA-256、分片成员、worker rank 和 mapper partial。
每个 Python stage 有超时，脚本上限为 1 MiB；任何 mapper 失败都会使全局处理失败，
不会产生成功结果。

五个内建指标仍作为无需脚本的快捷 processor：

`--count runs|steps|tool-calls|llm-calls|copied-context-steps` 使用两级聚合协议：pPilot
把确定性分片投递给 Pulsing analysis workers，每个 worker 用 pChronicle 规范化本地
分片并返回 partial count，driver 校验分片完整性后做 checked sum，最终
`results.jsonl` 只包含一条全局结果。五个指标依次用于轨迹量、交互步数、工具调用量、
LLM 实际调用量和复制上下文开销。未处于
torchrun 环境时会建立本机 Pulsing worker fleet；处于 torchrun 环境时，每个 rank
提供一个跨机 worker，只有 rank 0 读取输入、写结果并打印报告。

```bash
ppilot process ./atif --output ./analysis --mappers 16 --count steps

# 两台机器由 torchrun 启动同一命令；MASTER_ADDR/MASTER_PORT/RANK/WORLD_SIZE
# 决定 Pulsing 集群。非 driver rank 不需要访问输入和输出路径。
torchrun --nnodes 2 --nproc-per-node 1 --node-rank "$NODE_RANK" \
  --master-addr "$MASTER_ADDR" --master-port "$MASTER_PORT" --no-python \
  ppilot process ./atif \
  --output ./analysis --mappers 16 --count tool-calls
```

当前联邦协议只开放可安全 checked-sum 合并的五个类型化指标，其中 `llm-calls` 是
`SUM(llm_call_count)`，其余是行数或过滤行数。任意 SQL 应使用 `ppilot analysis`；SQL
不会被自动重写为分布式物理计划。

## 构建边界

公共 binary 使用 `cli` feature，它会启用 pChronicle/Lance/DataFusion；默认 pPilot
library 构建不启用该 feature，嵌入式调度器不会承担查询依赖：

```bash
cargo build -p persisting-ppilot --features cli --bin ppilot
```
