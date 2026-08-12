# `ppilot` — Run 编排与轨迹分析 CLI

统一入口是 `persisting batch` 与 `persisting query`。`ppilot` 仍作为组件级入口保留，
用于独立部署、调试和自检。pPilot 负责批量 Run 编排与查询交互；轨迹格式、
Lance/ATIF datasource 与 SQL 执行继续由 pChronicle library 提供。

## 命令

```text
ppilot run <SCRIPT> [OPTIONS]
ppilot chronicle import <INPUT> <STORE> [--content-offload-threshold BYTES] [--content-preview-bytes BYTES] [--content-zstd-level LEVEL]
ppilot chronicle export <STORE> <OUTPUT_DIR> --format openai_msg
ppilot convert <INPUT> <OUTPUT> [--from auto|atif|actf|openai_msg|storyline|agenticmd|lance] --to atif|actf|openai_msg|storyline|agenticmd|lance
ppilot query sql [<INPUT>] [--dataset <NAME=URI>]... [--dataset-file <FILE>] [--dataset-errors strict|report] (--sql <SQL> | --sql-file <FILE|->) [--source auto|lance|atif|openai_msg|actf] [--content-read-mode full|preview] [--table NAME=FORMAT:PATH]... [--max-files N] [--max-entries N] [--max-file-bytes N] [--max-record-bytes N] [--max-concurrent-files N] [--cache-bytes N] [--cache-files N] [--batch-size N] [--memory-limit-bytes N] [--spill-path DIR] [--max-spill-bytes N] [--timeout-seconds N] [--max-output-rows N] [--query-metrics]
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
persisting query <INPUT> (--sql <SQL> | --sql-file <FILE|->) [--source auto|lance|atif|openai_msg|actf] [--table NAME=FORMAT:PATH]...
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

把 ATIF JSON/JSONL、ACTF JSON 或 OpenAI-message JSON 校验并规范化为 Storyline，然后按
`session_id` 原子写入三表 Lance store。OpenAI 输入可为现有 `session_steps` 信封或包含
多个 session 的裸 step 数组。已存在的 session 被替换，其他 session 保留；Lance 三表
schema 不因输入格式而变化。`STORE` 可以是本地路径或 pChronicle 支持的对象存储 URI。

```bash
ppilot chronicle import ./trajectories.ndjson ./storyline-store
ppilot chronicle import ./openai-data ./storyline-store --format openai_msg
ppilot chronicle import ./task.actf.json ./storyline-store --format actf
ppilot chronicle import ./atif-directory s3://trajectory-bucket/storylines
```

超过阈值的内容列值会以内容地址写入共享 `objects.lance`，三表中保留带类型、长度、摘要和
头部 preview 的 descriptor。默认 threshold 为 64 KiB、preview 为 256 bytes、zstd level
为 3；可在导入时显式调整。例如以下命令让 4 KiB 以上的内容进入对象表：

```bash
ppilot chronicle import ./trajectories.ndjson ./storyline-store \
  --content-offload-threshold 4096 \
  --content-preview-bytes 256 \
  --content-zstd-level 3
```

OpenAI corpus 的完整原 row、源文件分组和 row ordinal 保存在既有 JSON 扩展列中，可按
原文件分组执行 JSON 数据模型级无损恢复：

```bash
ppilot chronicle export ./storyline-store ./recovered --format openai_msg
```

恢复不承诺空白和对象键顺序逐字节一致；不是由保真 OpenAI 导入产生的 Storyline 会被
拒绝，而不会自动降级成近似输出。

### `convert`

`convert` 是 pPilot 的独立格式转换入口，通过 Storyline hub 在 ATIF、ACTF、OpenAI messages、
Storyline JSON、AgenticMD 和三表 Lance 之间转换。文档格式的 `OUTPUT` 始终是目录，每条
trajectory 写一个文件；OpenAI 保真输入会恢复原文件分组。`lance` 输出使用现有
`StorylineLanceStore` 的原子 session 替换语义。

```bash
# 自动识别 OpenAI corpus 并写入三表 Lance
ppilot convert ./openai-data ./storyline-store --to lance

# 从三表恢复 OpenAI 原始文件分组
ppilot convert ./storyline-store ./recovered --from lance --to openai_msg

# 将 ATIF corpus 转为逐 session 的 Storyline JSON
ppilot convert ./atif-data ./storylines --from atif --to storyline

# ACTF 经三表 Lance 后按原 task/attempt 结构恢复
ppilot convert ./task.actf.json ./actf-store --to lance
ppilot convert ./actf-store ./recovered-actf --from lance --to actf
```

文档输出默认拒绝覆盖已有文件，可通过 `--force` 显式覆盖。对象存储 URI 仅用于 Lance
输入或输出；Lance-to-Lance 不属于格式转换，会被拒绝。

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

# 只读取 descriptor 中的内容头部，不访问 objects.lance 的完整 payload
ppilot query sql ./storyline-store --content-read-mode preview \
  --sql "SELECT session_id, message_json FROM steps LIMIT 20"

ppilot query sql s3://trajectory-bucket/persisting/storylines \
  --sql "SELECT COUNT(*) AS runs FROM runs"

# 一个查询快照挂载多个 Dataset；每个名称成为 SQL schema
ppilot query sql \
  --dataset current=./capture \
  --dataset archive=s3://trajectory-bucket/archive \
  --sql "SELECT dataset_name, runs FROM (
           SELECT 'current' AS dataset_name, COUNT(*) AS runs FROM current.runs
           UNION ALL
           SELECT 'archive', COUNT(*) FROM archive.runs
         ) ORDER BY dataset_name"

# 位置参数始终是名为 dataset 的默认 Dataset，可与额外挂载共存
ppilot query sql ./capture --dataset archive=s3://trajectory-bucket/archive \
  --sql "SELECT COUNT(*) FROM runs"

ppilot query sql ./trajectories.ndjson --sql-file analysis.sql
cat analysis.sql | ppilot query sql ./storyline-store --sql-file -

# ATIF/OpenAI/ACTF 文件或目录会自动识别；_file_ 是相对输入目录的虚拟列
ppilot query sql ./openai-data \
  --sql "SELECT _file_, COUNT(*) AS steps
         FROM steps WHERE _file_ LIKE 'cybergym_0729%'
         GROUP BY _file_ ORDER BY _file_"

ppilot query sql ./actf-data \
  --source actf \
  --sql "SELECT session_id, _file_ FROM runs WHERE _file_ LIKE 'bench/%'"

# 限制 manifest、单文件大小、解析并发和缓存，并把计数器输出到 stderr
ppilot query sql ./openai-data \
  --max-files 200000 --max-entries 400000 --max-file-bytes 67108864 \
  --max-record-bytes 16777216 \
  --max-concurrent-files 4 --cache-bytes 536870912 --cache-files 256 \
  --memory-limit-bytes 2147483648 --spill-path /var/tmp/ppilot \
  --max-spill-bytes 10737418240 \
  --timeout-seconds 600 --max-output-rows 10000000 \
  --query-metrics --sql "SELECT COUNT(*) FROM runs"

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

#### Dataset Catalog

完整的架构、发现算法、一致性与写入边界见
[pChronicle Dataset Catalog 设计](dataset-catalog.md)。

`sql` 在查询开始时为所有挂载建立一个不可变的、仅存在于本次查询中的 Catalog 快照。
快照固定成员和版本描述，但不在构建期打开全部 Lance dataset 或下载全部远程对象。它不是
需要维护的第二份元数据服务。每个 Dataset 名称是一个 DataFusion schema，并稳定提供
`sources`、`runs`、`steps`、`tool_calls`、`events` 和 `trajectories`：

- `sources` 每个发现源一行，包含 `_file_`、格式、类型、固定版本/ETag、本地 fingerprint、
  大小、修改时间、状态和错误；
- `runs`、`steps`、`tool_calls` 是统一 Storyline 投影，`trajectories` 是聚合视图；
- `events` 只对 canonical `events.lance` 有行，其他外围格式仍可通过规范化表查询；
- 所有数据表保留 Dataset 内相对的 `_file_`。一条轨迹的 Catalog 身份为
  `(Dataset, _file_, run_id)`。

位置参数 `<INPUT>` 始终挂载为 `dataset`，且 `runs` 等不带 schema 的旧 SQL 会解析到
`dataset.runs` 等兼容视图。它可以与 `--dataset archive=...` 同时使用。没有位置参数时，
单个命名挂载会成为默认 schema；两个及以上命名挂载没有隐式默认值，SQL 必须写
`current.runs`、`archive.steps` 这样的限定名。Dataset 名称不区分大小写并规范化为小写，
且必须匹配 `[A-Za-z_][A-Za-z0-9_]*`；重名会在发现前失败。

`--dataset` 可重复，也可使用 TOML：

```toml
[datasets]
current = "./capture"
archive = "s3://trajectory-bucket/archive"
```

```bash
ppilot query sql --dataset-file datasets.toml \
  --sql "SELECT _file_, run_id FROM current.runs LIMIT 20"
```

目录/对象前缀会递归发现 Storyline `CURRENT` store、canonical `events.lance`，以及
ATIF、OpenAI-message、ACTF 的 JSON/JSONL/NDJSON 文件；进入复合 store 后不再把其内部
文件当成独立源。不同 JSON 文件分别检测格式，因此一个 Dataset 可以混合外围格式。
默认 `--dataset-errors strict` 遇到快照构建期的坏候选即失败；`report` 会把这类错误写入
`<dataset>.sources` 并跳过该候选，同时把摘要输出到 stderr。延迟到 SQL 扫描期的打开、
远程条件读取、格式检测或解析错误始终让查询失败，不会静默漏行。

本地源在发现时冻结文件成员和 identity/size/mtime；Storyline 和 events store 固定到已
发布 generation/manifest revision；对象存储成员固定到 listing 结果，JSON object 读取使用
version/ETag precondition。每张 Dataset 数据表由 Catalog-aware provider 提供：它先根据
`_file_ =`、`IN`、`LIKE` 等条件裁剪 source，再按需打开命中的 Lance/file provider；一个
source 命中时物理计划不构造 `UnionExec`，多个命中时才组合。命中的远程对象才会以有界流
写入快照临时文件，未命中对象不会下载。业务列谓词继续下推到各原生 provider。刷新或下一
条查询才会看到新成员。
同一 Dataset 内多个源的内建表 join 必须包含 `_file_` 等值；跨 Dataset join 不要求两边
的 `_file_` 相同。

`--table` 可重复。支持 `csv`、`json`、`jsonl`（别名 `ndjson`）；`csv` 默认把首行
作为列名，`json` 表示一个 JSON 对象数组，`jsonl`/`ndjson` 表示每行一个 JSON 对象。
表名必须匹配 `[A-Za-z_][A-Za-z0-9_]*`，且不能覆盖内建表或先前注册的外部表。

Lance 输入默认使用 `--content-read-mode full`：投影到内容列时透明解析 descriptor、读取
`objects.lance`、解压并校验 hash/length，用户不会看到占位符。无需内容列的查询不会访问
对象 payload。`preview` 模式仍不暴露 descriptor，只把其中内嵌的有界头部作为内容列值，
适用于列表、抽样和人工预览；它会改变内容字段的值，不能用于要求完整 payload 的计算。
该选项只支持 Lance 输入，直接 JSON 输入必须读取原始文件里的实际内容。

直接查询 ATIF、OpenAI JSON 或 ACTF 时，`runs`、`steps`、`tool_calls` 都额外暴露只存在于
查询期的 `_file_` UTF-8 列。单文件输入的值是文件名；目录输入是使用 `/` 分隔的相对
路径，因此可用 SQL `LIKE` 的 `%`、`_` 通配符筛选。该列不会写入 Lance，也不会改变
三表物理 schema。`auto` 会按稳定路径顺序冻结递归发现的文件 manifest；执行时只有匹配
可下推 `_file_ =`、`IN` 或 `LIKE` 条件的文件才会执行格式检测、打开和规范化。被实际
扫描的混合格式、损坏或无法识别文件会报错，未命中的文件不会被读取。

manifest 会记录文件大小、修改时间和文件身份；首次读取前后都会校验，通常的替换或修改
会失败，而不是把不同版本静默混入结果。该检查不是内容哈希，能保留相同文件身份、大小
和修改时间的对抗性原地改写不在保证范围内。`--max-files`、`--max-entries`、
`--max-file-bytes`、`--max-record-bytes` 和 `--max-concurrent-files` 分别限制候选文件数、
目录遍历项、单文件输入体积、projected JSONL/NDJSON 单记录或 JSON array element 缓冲，
以及并发解析；
三张虚拟表共享按 Arrow 实际内存计量的 LRU 缓存和同文件 single-flight。ATIF
object/array/pretty JSON 与 JSONL/NDJSON 的 `steps` 投影查询会绕开完整缓存：NDJSON 逐记录
`BufRead`，array 逐 element 做结构扫描并经 slice decoder 解码，单 object 从 reader 解码；
三者都只构造计划需要的字段，并用
`session_id`、`step_id`、`source` 简单谓词提前裁剪；DataFusion 保留谓词再次校验。
该轻量路径校验 JSON、必需字段和当前表内约束；跨表引用完整性仍由导入路径或完整规范化
fallback 负责。
`--cache-bytes 0` 或 `--cache-files 0` 可关闭保留缓存。`--query-metrics` 除 cache 和源字节
外，还报告 projected files、streamed records、输入 buffer 峰值、scanned/pruned
documents/rows、emitted rows 和 projected Arrow bytes，不污染 stdout 的查询结果。

pPilot 以 Arrow batch 将 JSONL 结果直接流式写到 stdout，不再把完整结果集收集到内存；
`--timeout-seconds` 给异步规划和执行设置墙钟上限，`--max-output-rows` 限制输出行数。
同步 stdout 写入被操作系统阻塞时，异步 timeout 不能抢占该系统调用。超时、超行数或
后续分区失败前已经写出的 stdout 不会回滚，需要原子结果文件时应由调用方先写临时文件
并在成功后 rename。

`--memory-limit-bytes` 对 Lance 和直接文件查询使用同一个 DataFusion `FairSpillPool`，限制
join、sort、aggregate 等支持内存预留的执行算子；`--spill-path` 必须是已存在目录，
`--max-spill-bytes` 限制临时目录用量。DataFusion 并非所有分配都经过 memory pool，因此
它不是进程 RSS 的硬上限；容器/作业级 cgroup 限制仍是生产部署的最后一道边界。

多文件直接查询中，`session_id` 只保证在单个源文件内唯一。两个内建轨迹表 join 时必须
同时包含 `left._file_ = right._file_`，否则查询会在执行前拒绝，以避免同名 session 的
跨文件错误匹配。单表查询和与外部维表的 join 不受此限制。

直接 JSON 查询面向临时分析和受控批次。ATIF projected decoder 仍必须扫描源 JSON 字节，
但不会物化未引用字段；它不是文件内索引。超大规模、反复查询或对象存储数据应先用
`ppilot convert ... --to lance` 导入三表 Lance，再依赖 generation 快照、列裁剪、谓词
下推和索引。Dataset Catalog 统一的是查询命名、发现和快照边界，不会把外围 JSON 变成
可索引的持久元数据层。

完整可执行演示见
[`examples/pchronicle/06-query-openai-actf-directly`](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/06-query-openai-actf-directly)：
它同时验证目录自动识别、嵌套相对路径、`LIKE` 筛选，以及转换为 Lance 后物理 schema
仍不包含 `_file_`。

大对象 inline/offload 的精确物理体积、完整展开成本和 preview 分析路径见
[`examples/pchronicle/07-objects-lance-blob-offload`](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/07-objects-lance-blob-offload)。

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
