# pChronicle 命令行设计与使用指南

> 本文介绍 `pchronicle` 的公共命令行。当前安装版本的 `pchronicle --help` 是该版本可用参数的
> 准确信息来源。

`pchronicle` 用于浏览、查询、交换和服务 Agent Run Dataset。它既适合人在终端中探索，
也适合 shell、CI 和 Agent 发起受资源上限保护的只读分析。

## 按任务查找命令

- **先体验产品：** `pchronicle onboard query` 使用临时示例数据，不需要 Dataset 路径。
- **检查 Dataset：** 先用 `list`/`ls` 和 `stats overview`，再编写 SQL。
- **定位 Run 或文本：** 使用 `find --run-id`、`--session-id` 或 `--match`。
- **提出可复现问题：** 使用 `query --sql` 或 `query --file`，并显式设置输出与资源上限。
- **提供历史服务：** 先完成只读查询，再阅读[服务指南](../guides/serve.md)使用 `serve`。

第一次使用可以复制：

```bash
pchronicle onboard query
pchronicle list ./trajectory-data
pchronicle query ./trajectory-data --sql 'SELECT COUNT(*) FROM dataset.runs'
```

后两条命令要求你已有 Dataset；如果还没有数据，完成 `onboard query` 后继续阅读[Dataset 入门](../get-started.md)。

如果你第一次使用 pChronicle，可以从这里开始：

```bash
pchronicle onboard
```

## 1. Intro

### pChronicle

`pchronicle` 围绕 Dataset 提供一组组合式命令。读取命令可以浏览、定位和分析 Run；交换命令负责
导入和导出；`agent` 与 `serve` 分别提供交互分析和本地服务入口。

### Dataset

**Dataset 就是 path。** pChronicle 打开这条 path 作为轨迹存储。它可以写成：

- 本地目录或文件（`./local/path`）；
- 对象存储中的 URI 前缀（`s3://bucket/prefix`）；
- 解析到上述位置的 dataset pin（`@name`）。

Dataset 内部可以保存一种或多种受支持的运行数据格式。pChronicle 负责发现和规范化这些数据；用户只需要
向命令提供 Dataset，不需要先理解内部文件、分片、投影或版本布局。

每条读取命令都会使用一个内部一致的数据视图。命令开始后底层数据发生变化，不会改变该命令已经产生的结果。

`@NAME` 明确表示一个 dataset pin。裸字符串始终按路径或 URI 解释：

```text
prod 本地相对路径 ./prod
@prod 名为 prod 的 Dataset pin
```

这种区分可以避免同名目录出现或消失时，命令突然解析到不同位置。

### 命令总览

```text
pchronicle
├── onboard [SECTION] [DATASET]
├── dataset|ds pin|unpin|list|show|set|rename
├── list|ls [DATASET]
├── stats [DATASET]
├── stats overview|agents|models|tools [DATASET]
├── find [DATASET]
├── query [DATASET]
├── import --from SOURCE --to DATASET
├── sync --from DIRECTORY --to DIRECTORY --convert DIRECTORY
├── export --from DATASET --to TARGET
├── agent codex|claude [DATASET]
└── serve DATASET...
```

## 2. Commands

每节依次给出命令原型、一到两个代表性例子和必要说明。方括号表示可选参数，竖线表示互斥选择。

### 公共参数

```text
pchronicle [-c FILE] [--log-level LEVEL] <COMMAND> ...
```

| 参数 | 默认值 | 用途 |
|---|---:|---|
| `-c, --config FILE` | 平台配置目录 | 使用另一份用户配置文件 |
| `--log-level error|warn|info|debug` | `info` | 控制 stderr 诊断详细度；`serve` 同时用它过滤 Warehouse tracing（`pchronicle.serve`） |
| `-h, --help` | — | 查看当前层级帮助 |
| `-V, --version` | — | 查看版本 |

更多细节随各命令说明给出；任何层级都可以使用 `--help` 查看本机版本的准确语法。

### 2.1 `onboard`

```text
pchronicle onboard [SECTION] [DATASET] [--no-pause]
```

```bash
pchronicle onboard
pchronicle onboard query @prod
```

通过内建示例或自己的 Dataset 体验 pChronicle 工作流。`SECTION` 可以是 `all`、`concepts`、
`inspect`、`analyze`、`query`、`formats`、`find`、`exchange` 或 `serve`，默认为 `all`。
交互式终端会在章节之间暂停；pipe、重定向或 `--no-pause` 输出连续 Markdown。
完整引导还会演示统一的 FTS/JSONB `find` 表达式、Storyline Lance 导入导出以及只读 Web/API
边界；使用 `pchronicle onboard find DATASET` 可以直接查看检索语法。

### 2.2 `dataset`（pin）

```text
pchronicle dataset pin|unpin|list|show|set|rename …
```

```bash
pchronicle dataset pin default ./trajectory-data
pchronicle dataset show default
pchronicle dataset pin local ./trajectory-data
pchronicle dataset pin prod s3://bucket/evals
pchronicle dataset pin secure s3://bucket/evals --ak "$AWS_ACCESS_KEY_ID" --sk "$AWS_SECRET_ACCESS_KEY"
pchronicle dataset pin minio s3://bucket/evals --endpoint http://127.0.0.1:9000 --region us-west-2 --ak 123 --sk 123
pchronicle dataset pin regional s3://bucket/evals --region us-west-2
pchronicle dataset pin team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
pchronicle dataset set prod s3://new-bucket/evals
pchronicle dataset list
pchronicle stats @prod
```

`default` 是保留 pin：省略 Dataset 参数时使用，必须是本地目录。
其他 pin 只改用户配置，不移动或删除 Dataset。名称使用小写字母、数字、点、下划线和连字符，并以小写字母开头；`codex`/`claude`/`claude-code` 保留。
用户配置（`-c` / `PCHRONICLE_CONFIG`）使用 `[pins.<name>]`：

```toml
[pins.default]
uri = "/abs/path/to/warehouse"

[pins.prod]
uri = "s3://bucket/evals"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "..."
secret_key = "..."
```

旧键（如 `default_warehouse`、`aliases`）会被拒绝。`dataset list` 还会显示内置 `@codex` / `@claude` / `@claude-code`。
S3 凭证用 `--ak`/`--sk` 写在同一 pin 表中，不会被 `dataset list` / `dataset show` 打印。
`catalog://127.0.0.1:PORT` pin 是 Directory locator：`@team/prod` 换票打开 path，
`@team` 列出可访问 Datasets。详见 [RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md)。

### 2.3 `list`

```text
pchronicle list [DATASET] [--physical] [--format auto|table|json] [--errors report|strict]
 [--max-files N] [--max-entries N]
```

```bash
pchronicle list
pchronicle list @prod --physical --format json --errors strict
```

`list`（`ls`）显示 Dataset 中可独立查询的 Run 数据源，而不是底层 Lance fragment。`--physical` 增加大小、
修改时间和存储版本信息。还可以用 `--max-files` 和 `--max-entries` 限制发现范围。
`--errors report` 会报告坏数据项并继续；`strict` 遇到第一个坏数据项即失败。

### 2.4 `stats`

```text
pchronicle stats [DATASET] [--format auto|table|json] [--errors report|strict] [--timeout 30s]
 [--max-files N] [--max-entries N]
```

```bash
pchronicle stats
pchronicle stats @prod --errors strict --timeout 2m
```

结果包含 Dataset 的 `ready`、`degraded` 或 `error` 状态，各类数据计数、`counts_complete`，以及
canonical Event Store 的 Storyline projection 状态。还可以用 `--max-files` 和 `--max-entries`
限制检查范围。`stats` 不会创建、同步或修复 projection。

报告型子命令（原 `analysis`）挂在同一入口下：

```text
pchronicle stats <overview|agents|models|tools> [DATASET]
 [--format auto|table|jsonl|csv|tsv]
 [--limit 100] [--max-output-bytes 8MiB] [--timeout 30s]
```

```bash
pchronicle stats overview
pchronicle stats tools @prod --format csv --limit 20
```

| 报告 | 内容 |
|---|---|
| `overview` | 数据可用性，以及 Run、Step、Agent、Model、tool call 总览 |
| `agents` | 按 Agent identity 和 version 聚合 |
| `models` | 区分 Run 声明的 model 和实际观察到的 Step model |
| `tools` | 按 normalized function name 聚合，并报告 duration coverage |

内建报告用于常见、稳定的统计。需要任意筛选、join 或聚合时使用 `query`。

### 2.5 `find`

```text
pchronicle find [DATASET]
 (--run-id ID|--document-id ID|--session-id ID|--match EXPRESSION)
 [--source PATH] [--step-id N] [--match EXPRESSION ...]
 [--format auto|table|json] [--max-results N]
```

```bash
pchronicle find @prod --session-id session-42
pchronicle find ./dataset \
 --source nested/source.json \
 --session-id session-42 --step-id 7
pchronicle find ./dataset \
 --match "timeout" --match "retry" --format json
pchronicle find ./dataset \
 --match '$.tags=important' --match '$.priority=2' --format json
```

外部 ID 不保证在整个 Dataset 内唯一。没有 `--source` 时，同一个 ID 可以返回多个候选；结果中的
`source_path` 可以供下一次查询消除歧义。`--match` 是统一检索表达式：普通关键词搜索 Storyline
Step 内容并使用 FTS/Jieba 索引，`#system(prompt)` 等形式可以限定字段，`AND`、`OR`、`NOT` 用于
组合条件；`$.path=value`（或 `#json("$.path")=value`）按 JSONPath 对 JSONB 列做精确值匹配。仅
JSON 表达式当前搜索 Run 级 JSONB，和文本混合时搜索 Step 级 JSONB。可以重复 `--match` 要求所有表达式
同时满足；显式使用 `#json.metrics(...)` 时即使没有文本条件也会检索 Step 级 JSONB。CLI 与 Web
共用该表达式、报告的 `search.scope` 和 `snapshot_id`；Web UI 可以对返回字段做高亮和截取，
不改变命中集合。使用 `--format` 和 `--max-results` 控制结果形式和数量。每条结果还包含有界的 `preview`
摘要，便于在继续查询前判断候选是否正确。
JSON 输出还会报告 `search.mode`（`fts`、`json`、`fts+json` 或 `identity`）、`search.scope`（`steps` 或 `runs`）
以及 FTS 可用性和分词器元数据。
当前语法见 [Query Model](query-model.md)。
[RFC-0012](../../rfcs/0012-pchronicle-find-query-syntax.md) 是已接受的决策记录；与已安装
CLI 不一致时以 CLI 为准。

### 2.6 `query`

```text
pchronicle query [DATASET|--mount NAME=DATASET ...] (--sql SQL|--file FILE_OR_STDIN)
 [--format auto|table|jsonl|csv] [--output PATH_OR_STDOUT]
 [--max-output-rows N] [--max-output-bytes BYTES] [--timeout 30s]
```

```bash
pchronicle query ./dataset \
 --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
pchronicle query \
 --mount live=./live \
 --mount archive=@archive \
 --sql 'SELECT * FROM live.runs
 UNION ALL
 SELECT * FROM archive.runs'
```

`--file` 从文件读取 SQL，`--file -` 从 stdin 读取；`--format`、`--output`、输出上限和 `--timeout`
控制执行结果。一条命令只接受一条只读 statement，DDL、DML、COPY 和多语句会被拒绝。使用
`--mount` 后没有隐式 `dataset` schema，SQL 必须使用 mount 名。

### 2.7 `import`

```text
pchronicle import -f|--from SOURCE -t|--to NEW_DATASET
 [-i|--input-format FORMAT] [-o|--output-format preserve|storyline|compact-jsonl]
 [--replace] [--append] [--on-duplicate suffix|skip] [--yes]
 [--column NAME=JSON_PATH]... [--max-input-bytes BYTES]
```

```bash
pchronicle import \
 -f input.json -t ./imported -i atif
pchronicle import \
 -f ./corpus \
 -t s3://bucket/normalized \
 -o storyline
pchronicle import \
 -f more.json -t ./normalized --append --on-duplicate skip
pchronicle import \
 -f rebuilt.json -t ./normalized --replace --yes
pchronicle import \
 -f ./jsonl-root -t ./records.lance \
 -o compact-jsonl \
 --column id=$.event.id --column timestamp=$.event.time \
 --column model=$.payload.model
```

长参数分别是 `--from`、`--to`、`--input-format` 和 `--output-format`。短 option 始终只有一个字符，
因此格式参数使用 `-i` 和 `-o`，而不是 `-if` 和 `-of`。文件和目录默认自动识别输入格式；stdin
必须显式指定 `-i`。`preserve` 保留文件边界和相对路径，`storyline` 合并为 normalized Store；
对象存储目标必须使用 `storyline`。

| Format | Import | Export |
|---|---:|---:|
| `atif` | 是 | 是 |
| `actf` | 是 | 是 |
| `openai-messages` | 是 | 是 |
| `storyline` | 是 | 是 |
| `codex` | 是 | 否 |
| `claude-code` | 是 | 否 |
| `compact-jsonl` | 是 | 是 |

Codex 和 Claude Code session 是 decode-only 输入格式。Canonical Event Store 会自动识别并投影为
Storyline Dataset。默认创建要求目标不存在。`--append` 要求目标是已有 Storyline Dataset；
重复 `document_id` 默认增加 `#N` 后缀，也可用 `--on-duplicate skip` 跳过。`--replace` 会先将完整导入
写入临时路径，再将旧本地 Dataset rename 到备份路径、将新 Dataset rename 到正式路径，确认新路径
发布后才删除备份；因此必须交互确认或传入 `--yes`。对象存储 Dataset 的 replace 会先清空目标前缀再写入（非原子）。

Compact JSONL 是记录存储，不会转换或推断轨迹语义。指定
`--input-format compact-jsonl` 或 `--output-format compact-jsonl` 均会选择该格式。输入必须是本地
`.json`、`.jsonl` 或 `.ndjson` 文件或目录树；JSON object 和 array 都会被接受，每个 JSON 文档
会成为一条 record，数组会完整保留。缺失或无效的 `id` 和 `timestamp` 会获得稳定的
`source_filename#line_number` 值，默认路径分别为 `$.id` 和 `$.timestamp`。`--column id=PATH` 与
`--column timestamp=PATH` 用于覆盖默认路径，其他 `--column NAME=PATH` 会增加 nullable JSONB
投影列。Compact import 支持本地 `create` 和经确认的 `replace`，不支持 stdin、对象存储目标或
`append`。

### 2.8 `sync`

```text
pchronicle sync --from DIRECTORY --to DIRECTORY --convert DIRECTORY
 [--input-format FORMAT] [--column NAME=JSON_PATH]...
 [--interval DURATION] [--once]
```

`sync` 是常驻轮询器：监听源目录下的 `.json`、`.jsonl` 和 `.ndjson`。对于运行数据格式，它会将
变更合并到 pending 池，并按 `--interval` 将源文件逐字节批量镜像到本地 Warehouse 目录，同时将
数据转换为 Storyline Lance 写入 `--convert` 目标。一个批次成功后才清理 pending；失败会保留
变更并指数退避重试。`--once` 只执行一次初始批次后退出。当前目标必须是本地目录，两个目标
必须位于源目录之外。

指定 `--input-format compact-jsonl` 时，源目录必须是本地 `.json`、`.jsonl` 或 `.ndjson` 目录树，列映射规则与 Compact
import 相同。每个成功批次都会重新扫描整个目录，并原子替换 `--convert` 指向的 Compact Lance
快照，因此新增、修改和删除都会反映在下一快照中，但不提供行级增量更新。此模式仍要求传入
`--to` 作为兼容参数，但不会写入该路径。

### 2.9 `drop`

```text
pchronicle drop DATASET [--yes]
```

`drop` 永久删除本地 Dataset 目录或对象存储前缀。默认要求交互确认，`--yes` 可跳过确认；命令会
拒绝删除文件系统根目录或整个对象存储 bucket。

### 2.10 `export`

```text
pchronicle export -f|--from DATASET -t|--to TARGET -o|--output-format FORMAT
 [--source PATH] [--run-id ID|--document-id ID|--session-id ID] [--where EXPRESSION]
 [--strict] [--overwrite] [--max-trajectories N] [--max-output-bytes BYTES] [--timeout 30s]
```

```bash
pchronicle export \
 -f ./imported -t restored.json -o atif
pchronicle export \
 -f ./imported \
 -t - -o actf --session-id session-42 --strict
```

长参数分别是 `--from`、`--to` 和 `--output-format`。过滤条件包括 `--source`、`--run-id`、
`--document-id`、`--session-id` 和 `--where`；`--to -` 直接写 stdout。`--strict` 要求转换保留
原始 exchange document，失败时不产生部分输出。文件和对象存储输出默认 create-only，只有显式
`--overwrite` 才允许原子替换。

导出 Compact JSONL 时使用 `--output-format compact-jsonl`，目标必须是本地目录，且不支持
`--source`、ID 过滤或 `--where`，以保持原始 JSONL 文件的目录边界与字节内容。

### 2.11 `agent`

```text
pchronicle agent <codex|claude> [DATASET]
 [--ask QUESTION|--ask-file FILE_OR_STDIN] [--no-overview] [--dry-run]
```

```bash
pchronicle agent codex ./dataset
pchronicle agent claude @prod --ask '比较模型延迟'
```

默认先执行有界 `status` 和紧凑的 `stats overview`，再进入提问；`--no-overview` 只跳过
overview。问题也可以通过 `--ask-file` 从文件或 stdin 读取，`--dry-run` 用于预览启动内容。
Agent 注入是行为引导，不是 filesystem、network 或 tool permission 沙箱。

### 2.12 `serve`

```text
pchronicle serve
 [--listen LOOPBACK_ADDR] [--control LOOPBACK_ADDR] [--open]
 [--gateway ADDRESS --gateway-dataset DATASET [--gateway-split TEMPLATE]
 [--gateway-split-idle DURATION]]
 [--gateway-config FILE --gateway-dataset DATASET [--gateway-state DIRECTORY]]
 [--gateway-stream-markdown] [--gateway-debug]
 [--catalog-config FILE]
 [<[NAME=]DATASET> ...]
pchronicle serve catalog dataset add --catalog-config FILE NAME --uri URI [OPTIONS]
pchronicle serve catalog dataset remove --catalog-config FILE NAME...
pchronicle serve catalog dataset list --catalog-config FILE
pchronicle serve catalog issue --catalog-config FILE NAME
pchronicle serve catalog grant --catalog-config FILE NAME DATASET...
pchronicle serve catalog revoke --catalog-config FILE NAME DATASET...
```

```bash
pchronicle serve ./trajectory-data
pchronicle serve \
 --gateway auto \
 --gateway-dataset ./trajectory-data \
 --gateway-split '{user}/{date}/{hour}'
```

未指定服务 flag 时，只读 Web/API 默认监听 `127.0.0.1:0`。多个 Dataset 使用
`NAME=DATASET` mount；Control 模式要求名为 `default` 的 mount。`--catalog-config FILE`
会把文件中全部 `[datasets.*]` 挂进 Warehouse，并启用 `catalog://` locator；不能与位置参数
Dataset 同时使用。配合 `dataset pin NAME catalog://127.0.0.1:PORT --ak --sk`。
`pchronicle serve catalog dataset add|remove|list` 与 `issue|grant|revoke` 只改该文件、
不启动 HTTP；`issue` 把用户 sk 只打印一次。改 library、用户或授权后必须重启 serve。
`catalog` 是 `serve` 的保留子命令，挂载同名路径请用 `./catalog`。见
[RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md) 与
[RFC-0015](../../rfcs/0015-chronicle-manifest.md)。无需配置的 `--gateway`
在 `POST /v1/events` 接收 canonical trajectory events；`--gateway-dataset` 是自动挂载的
输出 URI，不再是 mount name。`--gateway-split` 支持 `{user}`、`{date}`、`{hour}`。
已有 canonical source 默认在最后一条事件后空闲 30 分钟才自动刷新 Storyline projection；
可用 `--gateway-split-idle DURATION` 覆盖。
Gateway 模式启用 Warehouse 后，单 trace 的事件、Storyline 和 trajectory 接口会读取已经发现
source 的最新 canonical manifest，正在进行中的 trace 不需要等待 projection 或全局 Catalog 刷新。
旧式转发 Gateway 仍可使用 `--gateway-config`，对象存储 capture 必须提供本地
`--gateway-state`。所有 listener 只允许
loopback；服务准备完成后，stdout 输出一行版本化 readiness JSON，endpoint 和诊断写 stderr。

#### Catalog 管理

Directory ACL 文件包含用户、datasets（libraries）和 grants。配置文件不存在时，管理命令会自动创建。

```text
pchronicle serve catalog issue --catalog-config FILE NAME
pchronicle serve catalog grant --catalog-config FILE NAME DATASET...
pchronicle serve catalog revoke --catalog-config FILE NAME DATASET...
pchronicle serve catalog dataset add --catalog-config FILE NAME --uri URI
 [--endpoint URL] [--region REGION] [--access-key KEY] [--secret-key KEY]
pchronicle serve catalog dataset remove --catalog-config FILE NAME...
pchronicle serve catalog dataset list --catalog-config FILE
```

`issue` 生成用户 AK/SK 并只显示一次 secret；`dataset add` 只登记 URI 与可选后端存储凭据，
不创建或删除对象存储数据；`grant`/`revoke` 增减该用户可打开的 library 名称（v1 是库成员关系，
不是细粒度 `--permission` 标志）。

### 公共输出与退出状态

stdout 只包含命令结果、导出内容或 readiness JSON；stderr 包含 Dataset 版本 metadata、warning、
进度和错误。`--log-level error` 可以关闭成功诊断，但不会改变 stdout 或退出码。

`auto` 在 TTY 中为 `dataset list`、`list`/`ls`、`stats`、`find` 选择 table，为 `query`、`stats` reports 选择 table；
相同命令在 pipe 中分别选择 JSON 和 JSONL。脚本中建议显式指定格式。

| Exit code | 含义 |
|---:|---|
| 0 | 命令按所选 error policy 完成 |
| 1 | 未分类内部错误 |
| 2 | 参数、输入或操作不合法 |
| 3 | 配置、Dataset、数据项或实体不存在 |
| 4 | create-only 目标或身份冲突 |
| 5 | 行数、字节数、文件数或队列资源超限 |
| 6 | timeout 或外部依赖暂时不可用 |

错误第一行带稳定 code，例如 `error[invalid_request]: --timeout must be greater than zero`。
`--log-level debug` 会增加脱敏后的原因链。颜色根据 TTY 和 `NO_COLOR` 自动决定，机器格式不含 ANSI。

## 3. Examples

### 从本地文件开始

```bash
pchronicle dataset pin local ./trajectory-data
pchronicle dataset pin default @local

pchronicle import \
 -f ./training.json \
 -t ./trajectory-data/training \
 -i openai-messages

pchronicle list
pchronicle stats
pchronicle stats overview
```

### 比较线上和归档 Dataset

```bash
pchronicle dataset pin live s3://bucket/live
pchronicle dataset pin archive s3://bucket/archive

pchronicle query \
 --mount live=@live \
 --mount archive=@archive \
 --sql 'SELECT model_name, COUNT(*) AS steps
 FROM (
 SELECT model_name FROM live.steps
 UNION ALL
 SELECT model_name FROM archive.steps
 )
 GROUP BY model_name
 ORDER BY steps DESC'
```

### 找到并严格导出一条 Run

```bash
pchronicle find @prod --session-id session-42 --format json

pchronicle export \
 -f @prod \
 -t session-42.actf.json \
 -o actf \
 --source nested/source.json \
 --session-id session-42 \
 --strict
```

### 在 CI 中使用

```bash
pchronicle \
 -c ./ci-config.toml \
 --log-level error \
 status ./fixtures \
 --format json > status.json

pchronicle \
 -c ./ci-config.toml \
 --log-level error \
 query ./fixtures \
 --file checks.sql \
 --format jsonl > checks.jsonl
```

定位后再写 SQL 见 [发现并查询](../guides/discover-and-query.md)，交换见
[导入与导出](../guides/exchange.md)，只读服务见 [本地服务 Dataset](../guides/serve.md)。
Snapshot 构造见 [Snapshot 设计](../design/catalog.md)。
