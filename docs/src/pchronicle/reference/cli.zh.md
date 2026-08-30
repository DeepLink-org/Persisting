# pChronicle 命令行设计与使用指南

> 本文介绍 `pchronicle` 的公共命令行。当前安装版本的 `pchronicle --help` 是该版本可用参数的
> 准确信息来源。

`pchronicle` 用于浏览、查询、交换和服务 Agent Run Dataset。它既适合人在终端中探索，
也适合 shell、CI 和 Agent 发起受资源上限保护的只读分析。

如果你第一次使用 pChronicle，可以从这里开始：

```bash
pchronicle onboard
```

## 1. Intro

### pChronicle

`pchronicle` 围绕 Dataset 提供一组组合式命令。读取命令可以浏览、定位和分析 Run；交换命令负责
导入和导出；`agent` 与 `serve` 分别提供交互分析和本地服务入口。

### Dataset

**Dataset 是 pChronicle 命令行操作的统一数据对象。** 它是一组可以被浏览、查询、分析、导入、
导出或提供服务的 Agent Run 数据。

一个 Dataset 可以表现为：

- 本地目录或文件（`./local/path`）；
- 对象存储中的 URI 前缀（`s3://bucket/prefix`）；
- 指向上述位置的用户 alias（`@alias-name`）。

Dataset 内部可以保存一种或多种受支持的运行数据格式。pChronicle 负责发现和规范化这些数据；用户只需要
向命令提供 Dataset，不需要先理解内部文件、分片、投影或版本布局。

每条读取命令都会使用一个内部一致的数据视图。命令开始后底层数据发生变化，不会改变该命令已经产生的结果。

`@NAME` 明确表示一个用户 alias。裸字符串始终按路径或 URI 解释：

```text
prod       本地相对路径 ./prod
@prod      名为 prod 的 Dataset alias
```

这种区分可以避免同名目录出现或消失时，命令突然解析到不同位置。

### 命令总览

```text
pchronicle
├── onboard [SECTION] [DATASET]
├── default show|set|clear
├── alias list|add|get-url|set-url|rename|remove
├── ls [DATASET]
├── status [DATASET]
├── find [DATASET]
├── query [DATASET]
├── analysis overview|agents|models|tools [DATASET]
├── import --from SOURCE --to DATASET
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
| `--log-level error|warn|info|debug` | `info` | 控制 stderr 诊断详细度 |
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

### 2.2 `default`

```text
pchronicle default <show|set LOCAL_DATASET|clear>
```

```bash
pchronicle default set ./trajectory-data
pchronicle default show
```

管理只读命令在省略 Dataset 时使用的本地默认 Dataset。
`set` 接受本地路径或解析为本地路径的 alias；目录不存在时会自动创建。`clear` 只删除默认配置，
不会删除 Dataset 数据。对象存储不能设为默认 Dataset。

### 2.3 `alias`

```text
pchronicle alias [list|add|remove|rename|get-url|set-url] [ARGUMENTS]
```

```bash
pchronicle alias add local ./trajectory-data
pchronicle alias add prod s3://bucket/evals
pchronicle alias add secure s3://bucket/evals --ak "$AWS_ACCESS_KEY_ID" --sk "$AWS_SECRET_ACCESS_KEY"
pchronicle alias add minio s3://bucket/evals --endpoint http://127.0.0.1:9000 --ak 123 --sk 123
pchronicle alias add regional s3://bucket/evals --region us-west-2
pchronicle alias
```

```bash
pchronicle alias set-url prod s3://new-bucket/evals
pchronicle status @prod
```

Alias 提供类似 `git remote` 的多 Dataset 管理方式，可以同时保存多个名称。`alias` 等价于
`alias list`，结果按名称排序。其他操作可用 `pchronicle alias --help` 查看。Alias 操作只修改用户配置，
不移动或删除 Dataset；名称使用小写字母、数字、点、下划线和连字符，并以小写字母开头。
`codex`、`claude`、`claude-code` 是保留名称。
`alias list` 还会始终显示系统内置的 `@codex`、`@claude`、`@claude-code`，它们分别指向对应的本地
Agent 会话目录。
对于 S3 Dataset，可以通过 `--ak` 和 `--sk` 配置访问密钥与秘密密钥；凭证与 URI 分开保存，
并在使用 alias 时通过标准 AWS 环境变量提供，不会由 `alias list` 或 `alias get-url` 输出。
对于 MinIO 等 S3 兼容服务，可以通过 `--endpoint` 保存服务地址；使用 alias 时会自动设置为
`AWS_ENDPOINT_URL_S3`。Dataset URI 仍应保持为 `s3://bucket/prefix`，不要把主机和端口写入 URI。
`alias set-url` 也支持相同的 `--endpoint` 参数；在两个 S3 URI 之间切换且未指定新 endpoint 时，
会保留原有 endpoint。
可选的 `--region` 也会按 alias 保存；省略时由 S3 客户端自行处理，需要回退时默认使用 `us-west-2`。

### 2.4 `ls`

```text
pchronicle ls [DATASET] [--physical] [--format auto|table|json] [--errors report|strict]
  [--max-files N] [--max-entries N]
```

```bash
pchronicle ls
pchronicle ls @prod --physical --format json --errors strict
```

`ls` 显示 Dataset 中可独立查询的 Run 数据源，而不是底层 Lance fragment。`--physical` 增加大小、
修改时间和存储版本信息。还可以用 `--max-files` 和 `--max-entries` 限制发现范围。
`--errors report` 会报告坏数据项并继续；`strict` 遇到第一个坏数据项即失败。

### 2.5 `status`

```text
pchronicle status [DATASET] [--format auto|table|json] [--errors report|strict] [--timeout 30s]
  [--max-files N] [--max-entries N]
```

```bash
pchronicle status
pchronicle status @prod --errors strict --timeout 2m
```

结果包含 Dataset 的 `ready`、`degraded` 或 `error` 状态，各类数据计数、`counts_complete`，以及
canonical Event Store 的 Storyline projection 状态。还可以用 `--max-files` 和 `--max-entries`
限制检查范围。`status` 不会创建、同步或修复 projection。

### 2.6 `find`

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
JSON 表达式搜索 Run 级 JSONB，和文本混合时搜索 Step 级 JSONB。可以重复 `--match` 要求所有表达式
同时满足；显式使用 `#json.metrics(...)` 时即使没有文本条件也会检索 Step 级 JSONB。使用 `--format` 和 `--max-results` 控制结果形式和数量。每条结果还包含有界的 `preview`
摘要，便于在继续查询前判断候选是否正确。
JSON 输出还会报告 `search.mode`（`fts`、`json` 或 `fts+json`）、`search.scope`（`steps` 或 `runs`）
以及 FTS 可用性和分词器元数据。
规范语法和执行语义见 [RFC-0012](../../rfcs/0012-pchronicle-find-query-syntax.md)。

### 2.7 `query`

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

### 2.8 `analysis`

```text
pchronicle analysis <overview|agents|models|tools> [DATASET]
  [--format auto|table|jsonl|csv] [--limit N] [--timeout 30s]
```

```bash
pchronicle analysis overview
pchronicle analysis tools @prod --format csv --limit 20
```

| Analysis | 内容 |
|---|---|
| `overview` | 数据可用性，以及 Run、Step、Agent、Model、tool call 总览 |
| `agents` | 按 Agent identity 和 version 聚合 |
| `models` | 区分 Run 声明的 model 和实际观察到的 Step model |
| `tools` | 按 normalized function name 聚合，并报告 duration coverage |

内建分析用于常见、稳定的报告。需要任意筛选、join 或聚合时使用 `query`。

### 2.9 `import`

```text
pchronicle import -f|--from SOURCE -t|--to NEW_DATASET
  [-i|--input-format FORMAT] [-o|--output-format preserve|storyline] [--max-input-bytes BYTES]
```

```bash
pchronicle import \
  -f input.json -t ./imported -i atif
pchronicle import \
  -f ./corpus \
  -t s3://bucket/normalized \
  -o storyline
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

Codex 和 Claude Code session 是 decode-only 输入格式。Canonical Event Store 会自动识别并投影为
Storyline Dataset。导入目标必须是新 Dataset；任一必要输入失败时，最终目标不会发布。

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

### 2.11 `agent`

```text
pchronicle agent <codex|claude> [DATASET]
  [--ask QUESTION|--ask-file FILE_OR_STDIN] [--no-overview] [--dry-run]
```

```bash
pchronicle agent codex ./dataset
pchronicle agent claude @prod --ask '比较模型延迟'
```

默认先执行有界 `status` 和紧凑的 `analysis overview`，再进入提问；`--no-overview` 只跳过
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
  [<[NAME=]DATASET> ...]
```

```bash
pchronicle serve ./trajectory-data
pchronicle serve \
  --gateway auto \
  --gateway-dataset ./trajectory-data \
  --gateway-split '{user}/{date}/{hour}'
```

未指定服务 flag 时，只读 Web/API 默认监听 `127.0.0.1:0`。多个 Dataset 使用
`NAME=DATASET` mount；Control 模式要求名为 `default` 的 mount。无需配置的 `--gateway`
在 `POST /v1/events` 接收 canonical trajectory events；`--gateway-dataset` 是自动挂载的
输出 URI，不再是 mount name。`--gateway-split` 支持 `{user}`、`{date}`、`{hour}`。
已有 canonical source 默认在最后一条事件后空闲 30 分钟才自动刷新 Storyline projection；
可用 `--gateway-split-idle DURATION` 覆盖。
Gateway 模式启用 Warehouse 后，单 trace 的事件、Storyline 和 trajectory 接口会读取已经发现
source 的最新 canonical manifest，正在进行中的 trace 不需要等待 projection 或全局 Catalog 刷新。
旧式转发 Gateway 仍可使用 `--gateway-config`，对象存储 capture 必须提供本地
`--gateway-state`。所有 listener 只允许
loopback；服务准备完成后，stdout 输出一行版本化 readiness JSON，endpoint 和诊断写 stderr。

### 公共输出与退出状态

stdout 只包含命令结果、导出内容或 readiness JSON；stderr 包含 Dataset 版本 metadata、warning、
进度和错误。`--log-level error` 可以关闭成功诊断，但不会改变 stdout 或退出码。

`auto` 在 TTY 中为 `alias`、`ls`、`status`、`find` 选择 table，为 `query`、`analysis` 选择 table；
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
pchronicle alias add local ./trajectory-data
pchronicle default set @local

pchronicle import \
  -f ./training.json \
  -t ./trajectory-data/training \
  -i openai-messages

pchronicle ls
pchronicle status
pchronicle analysis overview
```

### 比较线上和归档 Dataset

```bash
pchronicle alias add live s3://bucket/live
pchronicle alias add archive s3://bucket/archive

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
