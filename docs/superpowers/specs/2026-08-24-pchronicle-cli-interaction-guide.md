# pChronicle CLI 严格交互指南

日期：2026-08-24  
状态：已实现 canonical CLI；兼容参数处于迁移期  
范围：公共 `pchronicle` 命令行的命令树、参数、输入输出、错误、常见场景与兼容迁移  
非目标：HTTP API、Web UI、SQL schema、存储布局、Gateway TOML 字段、Search、TTAS、Queue、Sampler、`dlcapt`

面向用户的整体设计、使用流程和示例见
[`pChronicle 命令行设计与使用指南`](../../src/pchronicle/reference/cli.zh.md)。本文只保留实现与验收契约。

本文使用 RFC 2119 风格的规范词：

- **MUST / MUST NOT**：公共 CLI 契约；实现和文档不得偏离。
- **SHOULD / SHOULD NOT**：默认应遵循；偏离必须有明确理由。
- **MAY**：可选能力，不构成调用方依赖。

## 1. 设计目标

公共 CLI 必须同时服务三类调用方：

1. 人在终端中的探索和诊断；
2. shell、CI 和数据流水线中的自动化；
3. Codex、Claude 等 Agent 发起的只读分析。

本设计遵守以下原则：

1. **一条命令只有一种主语义。** 可选参数不得让同一位置参数在不同上下文中改变含义。
2. **读取可省略 Dataset，写入必须显式。** 只读命令可以使用 default Warehouse；创建、覆盖、监听和写入目标必须在命令中明确出现。
3. **stdout 是结果，stderr 是诊断。** 机器结果、数据行和 readiness 不得与日志混写。
4. **默认有界。** Source discovery、查询结果、编码字节、执行时间和输入大小必须有非零有限默认值。
5. **失败不留下半成品。** 本地和对象存储输出遵守 create-only 或显式 overwrite，并在成功发布前使用 staging/CAS。
6. **自动化不依赖 TTY 猜测。** `auto` 只改善人在终端中的显示；脚本应显式选择格式。
7. **Source-local ID 不冒充全局 ID。** `run_id`、`session_id`、`document_id` 和 `step_id` 的输出必须保留 `source_path`。
8. **别名只用于人工兼容。** 文档、测试和自动化只使用 canonical spelling。

## 2. 术语

| 术语 | 严格含义 |
|---|---|
| Warehouse | 本地默认 Dataset 根；是配置，不是守护进程或隐藏数据库 |
| Dataset | 一次 catalog discovery 的 URI 根，可为本地目录或受支持的对象存储前缀 |
| Source | Dataset 内可独立冻结、识别和查询的逻辑轨迹来源 |
| Snapshot | 单次命令冻结的 Dataset/Source 版本集合 |
| Dataset URI | 本地路径、`file://` URI、对象存储 URI，或 CLI 展开的 `@codex` / `@claude` 别名 |
| Source path | Dataset 内的相对逻辑路径；不是操作系统任意路径 |
| Result | stdout 上的表格、JSON、JSONL、CSV、导出文档或 readiness JSON |
| Diagnostic | stderr 上的 snapshot metadata、warning、progress 或 error |

## 3. 顶层语法

```text
pchronicle [GLOBAL_OPTIONS] <COMMAND> [COMMAND_OPTIONS]
```

### 3.1 公共命令树

```text
pchronicle
├── onboard [SECTION]
├── default
│   ├── show
│   ├── set <LOCAL_DATASET>
│   └── clear
├── alias
│   ├── list
│   ├── add <NAME> <DATASET>
│   ├── get-url <NAME>
│   ├── set-url <NAME> <DATASET>
│   ├── rename <OLD> <NEW>
│   └── remove <NAME>
├── ls
├── status
├── query
├── analysis
│   ├── overview
│   ├── agents
│   ├── models
│   └── tools
├── find
├── import
├── export
├── agent <codex|claude>
└── serve
```

`echo` 是 Gateway 集成测试工具，不属于用户 Dataset 工作流。目标形态为隐藏的
`pchronicle dev echo`，不出现在默认顶层帮助中，也不承诺长期兼容。

顶层帮助 SHOULD 按以下顺序分组，而不是按字母排序：

1. Learn：`onboard`
2. Inspect：`ls`、`status`、`find`
3. Analyze：`query`、`analysis`、`agent`
4. Exchange：`import`、`export`
5. Operate：`serve`
6. Configure：`default`、`alias`

### 3.2 公共参数

| 参数 | 默认值 | 契约 |
|---|---:|---|
| `-c, --config <FILE>` | 平台配置目录 | 覆盖用户配置文件；优先级高于 `PCHRONICLE_CONFIG` |
| `--log-level <error|warn|info|debug>` | `info` | 统一控制 stderr 诊断详细度；不得改变 stdout 或退出码 |
| `-h, --help` | — | 打印当前层级帮助并退出 0 |
| `-V, --version` | — | 打印版本并退出 0 |

`--log-level` 的严格语义为：

| Level | stderr 内容 |
|---|---|
| `error` | 最终错误；成功命令保持静默 |
| `warn` | `error` + 可行动 warning |
| `info` | `warn` + snapshot、Dataset URI、行数、字节数和 readiness metadata |
| `debug` | `info` + 脱敏后的完整 cause chain 和有界内部诊断 |

最终错误不是可关闭的日志，任何 level 下都必须输出。颜色不再提供 CLI 参数：只有 human stderr/TTY、
且 `NO_COLOR` 未设置时 MAY 使用 ANSI；所有 JSON、JSONL、CSV 和 pipe 输出 MUST NOT 含 ANSI。

全局参数 MUST 可出现在子命令之前或之后；文档中的 canonical order 始终为：全局参数、命令、
选择器、过滤器、预算、输出。

### 3.3 Dataset 参数与 alias 解析

当命令的主要操作对象是一个 Dataset 时，Dataset MUST 是第一个业务位置参数：

```text
pchronicle ls [DATASET]
pchronicle status [DATASET]
pchronicle query [DATASET] --sql SQL
pchronicle analysis <REPORT> [DATASET]
pchronicle find [DATASET] <IDENTITY_SELECTOR>
```

`DATASET` 接受本地路径、Dataset URI 或 `@NAME` alias。

- 只读命令省略 `DATASET` 时 MUST 使用 default Warehouse；未配置则返回 `not_found`。
- 显式位置参数始终优先于 default Warehouse。
- `query` 的多 Dataset 模式使用可重复的 `--mount <NAME=DATASET>`，且不得再传单 Dataset 位置参数。
- `serve` 不读取 default Dataset，必须接受一个或多个 `[NAME=]DATASET` 位置参数。
- `onboard [SECTION] [DATASET]` 与 `agent <AGENT> [DATASET]` 中，前一个位置参数已有固定类型，Dataset 放在其后。
- Dataset-primary 命令不提供 `-d, --dataset`；canonical 语法直接使用 Dataset 位置参数。

`import` 和 `export` 是双端传输操作，不适用单对象位置参数规则。两者 MUST 使用对偶的
`-f, --from <SOURCE>` 与 `-t, --to <TARGET>`，使数据方向在命令文本中始终显式。

短 option 严格为一个 ASCII 字符。因此格式参数使用 `-i, --input-format` 和
`-o, --output-format`；`-if` / `-of` MUST 被拒绝，不得通过 argv 预处理实现非标准多字符短 option。

Alias 使用显式 `@NAME` 引用。裸字符串 `prod` 始终是本地相对路径，`@prod` 始终是 alias；实现不得根据
“当前是否存在同名 alias 或目录”改变解析结果。`@NAME/SUFFIX` 表示 alias Dataset 下的相对子路径，
`SUFFIX` 必须拒绝空段、`.`、`..`、反斜线和 NUL。

### 3.4 通用预算参数

读取 Dataset 的命令复用以下名字和含义：

| 参数 | 默认值 | 适用范围 |
|---|---:|---|
| `--max-files <N>` | 存储层公共默认值 | catalog discovery 最多接受的逻辑 Source 数 |
| `--max-entries <N>` | 存储层公共默认值 | discovery 最多检查的文件系统项或对象数 |
| `--timeout <DURATION>` | `30s` | discovery 之后的命令主操作总时限 |
| `--max-output-bytes <BYTES>` | 命令特定 | 完整编码结果上限 |

数值 MUST 大于零。`DURATION` 接受 `ms`、`s`、`m`、`h`；无单位值 MUST 被拒绝，避免调用方误解。
字节值接受整数和 `KiB`、`MiB`、`GiB` 后缀。帮助必须同时显示人类值和解析后的字节值。

## 4. 跨命令交互契约

### 4.1 stdout 与 stderr

- 成功结果 MUST 只写 stdout。
- snapshot ID、Dataset URI、结果行数、字节数、warning 和进度 MUST 只写 stderr。
- 失败时，未声明可流式部分成功的命令 MUST NOT 在 stdout 留下部分结果。
- `--log-level error` MUST 保留 stdout，只抑制成功诊断和 warning。
- 管道关闭导致的 broken pipe SHOULD 视为正常提前结束，不打印 Rust error chain。

### 4.2 输出格式

| 输出类别 | `auto` 在 TTY | `auto` 在 pipe/file |
|---|---|---|
| `alias`、`ls`、`status`、`find` | `table` | `json` |
| `query`、`analysis` | `table` | `jsonl` |
| `import` result | `json` | `json` |
| `serve` readiness | 单行 JSON | 单行 JSON |
| `export` document | 不允许 `auto` | 不允许 `auto` |

脚本 MUST 显式传结果格式：查询类命令使用 `--format`，import 使用 `--input-format` / `--output-format`，
export 使用 `--output-format`。JSON object 输出必须包含 `schema_version`；新增 optional
字段不升 major，删除字段、改变类型或改变字段语义必须升 major。JSONL 每行是一个独立 object。

表格只面向人，不是兼容接口。列顺序可以在 minor version 变化，但列名变化 SHOULD 在 release note 中说明。

### 4.3 文件与流

- `-` 在输入位置唯一表示 stdin，在输出位置唯一表示 stdout。
- `--from -` 自动表示有限 stdin 输入，不再额外要求 `--stream`。
- `export --to -` 自动表示 stdout，不再额外要求 `--stream`；`query --output -` 仍表示查询结果写 stdout。
- stdin 输入必须显式指定格式，因为没有文件名可用于 detection。
- 普通文件输出默认 create-only；目标存在返回 `conflict`。
- `--overwrite` 只允许在支持原子替换的文件/对象输出上使用。
- 发布成功前 MUST 不可观察到最终路径的空文件、部分内容或半个 Dataset。

### 4.4 错误模型与退出码

| 退出码 | code | 含义 |
|---:|---|---|
| 0 | — | 按调用方选择的 error policy 完成 |
| 1 | `internal` | 未分类内部错误 |
| 2 | `invalid_request` / `unsupported` | 参数、输入或操作不合法 |
| 3 | `not_found` | 配置、Dataset、Source 或显式实体不存在 |
| 4 | `conflict` | create-only 目标已存在、身份冲突、CAS 冲突 |
| 5 | `resource_exhausted` | 行数、字节数、Source 数或队列上限超出 |
| 6 | `unavailable` | timeout、依赖服务或对象存储暂时不可用 |

最终错误使用稳定的单行前缀，例如 `error[invalid_request]: --timeout must be greater than zero`。
`--log-level debug` MAY 在后续 stderr 行增加 cause chain，但 URI credential、HTTP authorization、
Gateway body 和环境变量 secret MUST 在任何 level 下脱敏。

`--errors report` 表示“将坏 Source 作为数据报告并继续”，所以 degraded 结果仍可退出 0；结果中的
`counts_complete`、Source status 和 warning 是契约的一部分。`--errors strict` 遇到任一坏 Source 必须非零退出。

### 4.5 安全与信任边界

- SQL 只允许一条 read-only statement；DDL、DML、COPY 和多语句 MUST 被拒绝。
- Dataset 内容、Source path、模型消息和 `--ask` 内容都是不可信数据，不得作为本地命令执行。
- URI MUST 拒绝嵌入的用户名、密码、query 和 fragment；凭证只从受支持的环境/provider chain 获取。
- 未认证 Warehouse、Control、Gateway 和 Echo listener MUST 只允许 loopback。
- `--where` 是受限于 `trajectories` view 的 SQL expression，不是 shell expression。

## 5. 子命令规范

## 5.1 `onboard`

用途：用内建 Dataset 或显式 Dataset 演示公共工作流；不会修改显式 Dataset。

```text
pchronicle onboard [SECTION] [DATASET] [--no-pause]

SECTION := all | concepts | inspect | analyze | query | formats | find | exchange | serve
```

规则：

- 默认 `SECTION=all`。
- `concepts`、`formats`、`exchange`、`serve` 不接受 `DATASET`。
- 只有 stdin 和 stdout 都是 TTY、section 为 `all` 且未传 `--no-pause` 时才暂停。
- pipe 输出 MUST 是无 ANSI 的 Markdown。
- 内建示例和临时 Warehouse MUST 在退出后清理。

常见场景：

```bash
# 首次完整体验
pchronicle onboard

# 对自己的 Dataset 走完整流程
pchronicle onboard ./trajectory-data

# 只学习查询
pchronicle onboard query @evals

# CI 中生成无交互 Markdown
pchronicle onboard --no-pause > walkthrough.md
```

拒绝案例：`pchronicle onboard concepts ./data`，因为 concepts 不读取 Dataset。

## 5.2 `default`

用途：管理本地 default Warehouse 配置；不管理对象存储，不启动服务。

```text
pchronicle default show
pchronicle default set <LOCAL_DATASET>
pchronicle default clear
```

规则：

- `show` 输出 canonical absolute path；未配置返回 `not_found`。
- `set` 接受本地路径或解析为本地路径的 `@NAME`；可创建不存在的目录，随后原子更新用户配置。
- `set` MUST 拒绝对象存储 URI、普通文件和不可 canonicalize 的目录。
- `clear` 只删除 default Warehouse 配置，不删除 Warehouse 目录或其中数据。
- mutation metadata 写 stderr；最终路径或清理结果写 stdout。

常见场景：

```bash
pchronicle alias add local ./trajectory-data
pchronicle default set @local
pchronicle default show
pchronicle --config ./ci-config.toml default set ./fixtures
pchronicle default clear
```

拒绝案例：`pchronicle default set s3://bucket/data`。

## 5.3 `alias`

用途：在用户配置中管理多个命名 Dataset，交互模型类似 `git remote`。

```text
pchronicle alias [list] [--format auto|table|json]
pchronicle alias add <NAME> <DATASET>
pchronicle alias get-url <NAME>
pchronicle alias set-url <NAME> <DATASET>
pchronicle alias rename <OLD> <NEW>
pchronicle alias remove <NAME>
```

规则：

- `pchronicle alias` 等价于 `pchronicle alias list`。
- alias name 必须匹配 `[a-z][a-z0-9._-]{0,63}`，只接受 portable lowercase ASCII。
- `codex`、`claude`、`claude-code` 保留给内建 session alias，用户不得覆盖。
- `list` 只列出用户配置中的 alias；内建 session alias 在帮助末尾单独列出，不混入可修改结果。
- `add` 必须拒绝已存在的 name；多个 name MAY 指向同一 Dataset。
- `set-url` 只修改已存在 alias；不存在返回 `not_found`。
- `rename` 必须原子完成；目标 name 已存在返回 `conflict`。
- `remove` 只删除 alias，不删除 Dataset 或 default Warehouse；不存在返回 `not_found`。
- alias target 在写入配置前必须完成语法校验和归一化；不得保存另一个 `@NAME`，从而避免递归和环。
- `list` 默认按 name 排序；table 输出 `NAME`、`DATASET` 两列，JSON 输出版本化 alias 数组。
- 所有 mutation 必须原子更新 `-c, --config` 选择的用户配置文件。

常见场景：

```bash
pchronicle alias add local ./trajectory-data
pchronicle alias add prod s3://bucket/evals
pchronicle alias
pchronicle alias get-url prod
pchronicle status @prod
pchronicle query @prod --sql 'SELECT COUNT(*) FROM dataset.runs'
pchronicle alias set-url prod s3://new-bucket/evals
pchronicle alias rename prod production
pchronicle alias remove production
```

拒绝案例：

- `alias add prod @local`：alias 不允许指向 alias；
- `alias add codex ./data`：内建名称保留；
- `alias add Prod ./data`：name 不满足 portable lowercase contract；
- `alias rename a b` 且 `b` 已存在。

## 5.4 `ls`

用途：发现 Dataset 下的逻辑 Source，不展开 Lance fragment。

```text
pchronicle ls [DATASET]
  [--physical]
  [--format auto|table|json]
  [--errors report|strict]
  [--max-files N] [--max-entries N]
```

规则：

- canonical spelling 是 `ls`；`list` 仅为兼容 alias。
- 默认 `--errors report`。
- `--physical` MAY 增加 size、mtime、version/snapshot ref，不改变逻辑 Source 数。
- JSON 必须包含 Dataset URI、Snapshot ID、创建时间和每个 Source 的 status。

常见场景：

```bash
pchronicle ls
pchronicle ls ./dataset
pchronicle ls @prod --format json
pchronicle ls s3://bucket/prefix --physical --errors strict
```

异常场景：

- discovery 超限：`resource_exhausted`；不得悄悄截断。
- 单个 Source 损坏且 `report`：返回 degraded Source，退出 0。
- 单个 Source 损坏且 `strict`：不输出部分 JSON，非零退出。

## 5.5 `status`

用途：报告 Snapshot 健康、聚合计数和自动 Storyline projection 状态；只读，不修复。

```text
pchronicle status [DATASET]
  [--format auto|table|json]
  [--errors report|strict]
  [--timeout 30s]
  [--max-files N] [--max-entries N]
```

规则：

- status 为 `ready|degraded|error`。
- `counts_complete=false` 时，计数必须标记 partial，缺失值不得被解释为零。
- projection 状态为 `fresh|stale|missing|error`。
- 命令 MUST NOT 创建、同步或重建 projection。

常见场景：

```bash
pchronicle status
pchronicle status ./dataset --format json
pchronicle status @evals --errors strict --timeout 2m
```

## 5.6 `query`

用途：对单 Dataset 或多个命名 mount 执行一条有界只读 SQL。

```text
pchronicle query
  [DATASET | --mount NAME=DATASET ...]
  (--sql SQL | --file FILE_OR_STDIN)
  [--format auto|table|jsonl|csv]
  [--output PATH_OR_STDOUT]
  [--max-output-rows N]
  [--max-output-bytes BYTES]
  [--timeout 30s]
  [--max-files N] [--max-entries N]
  [--overwrite]
```

规则：

- `--sql`、`--file` 必须恰好一个；`--file -` 从 stdin 读取 SQL。
- 单 Dataset 模式 schema 固定为 `dataset`。
- 多 mount 模式没有隐式 `dataset` schema，SQL 必须使用 mount 名。
- `--mount` 可重复，name 必须是合法且唯一的 SQL identifier。
- 默认 `--output -`。文件输出 create-only，`--overwrite` 显式允许原子替换。
- 行/字节上限应在完整结果写 stdout 或最终文件前验证，超限不得留下部分输出。

常见场景：

```bash
# default Warehouse
pchronicle query --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'

# 显式 Dataset
pchronicle query ./dataset \
  --sql 'SELECT session_id, COUNT(*) FROM dataset.steps GROUP BY session_id'

# 多 Dataset
pchronicle query \
  --mount live=./live \
  --mount archive=s3://bucket/archive \
  --sql 'SELECT * FROM live.runs UNION ALL SELECT * FROM archive.runs'

# 长 SQL 与机器输出
pchronicle query ./dataset --file report.sql \
  --format csv --output report.csv

# stdin SQL
printf '%s\n' 'DESCRIBE dataset.steps' | \
  pchronicle query ./dataset --file - --format jsonl
```

拒绝案例：

- 同时传 Dataset 位置参数与 `--mount`；
- 同时传 `--sql` 与 `--file`；
- SQL 中包含两条 statement；
- 输出超过预算；
- 文件已存在但未传 `--overwrite`。

## 5.7 `analysis`

用途：执行版本化、稳定语义的内建分析；任意分析使用 `query`。

```text
pchronicle analysis <overview|agents|models|tools>
  [DATASET]
  [--format auto|table|jsonl|csv]
  [--limit N]
  [--max-output-bytes BYTES]
  [--timeout 30s]
  [--max-files N] [--max-entries N]
```

规则：

- `overview`：Source readiness、trajectory、Step、Agent、Model、tool call 总览。
- `agents`：按 Agent identity/version 聚合。
- `models`：区分 declared trajectory model 与 observed Step model。
- `tools`：按 normalized function name 聚合，并明确 duration coverage。
- `--limit` 默认 100，最大 10,000；更大结果使用 `query`。
- 输出必须带 analysis schema/version 或在 metadata 中报告 analysis definition version。
- `summary`、`tool-calls`、`toolcalls` 只保留为 alias，不用于文档和脚本。

常见场景：

```bash
pchronicle analysis overview
pchronicle analysis agents ./evals --limit 20
pchronicle analysis models @prod --format jsonl
pchronicle analysis tools ./evals --format csv > tools.csv
```

## 5.8 `find`

用途：按 Source-local ID 定位候选实体，不假设 ID 全局唯一。

```text
pchronicle find [DATASET]
  [--source SOURCE_PATH]
  (--run-id ID | --document-id ID | --session-id ID)
  [--step-id N]
  [--format auto|table|json]
  [--max-results N]
  [--max-output-bytes BYTES]
  [--timeout 30s]
  [--max-files N] [--max-entries N]
```

规则：

- 三种 identity selector 必须恰好一个。
- `--step-id` 只允许与 `--session-id` 同时使用。
- `--source` 是 Dataset-relative Source path；必须拒绝绝对路径、`.`、`..` 和 NUL。
- 无匹配是成功的空结果，退出 0；显式 Source 不存在返回 `not_found`。
- 多匹配是合法结果；每项必须包含 `source_path`。
- 超过 `--max-results` 时返回前 N 项并设置 `truncated=true`，不得伪装成完整结果。

常见场景：

```bash
pchronicle find --session-id session-42
pchronicle find ./dataset --run-id run-42
pchronicle find @prod --session-id session-42 --step-id 7
pchronicle find ./dataset --source nested/source.json --session-id session-42
```

## 5.9 `import`

用途：从交换文档、目录、stdin 或 canonical Event Store 创建一个新 Dataset。

```text
pchronicle import
  -f, --from PATH_URI_OR_STDIN
  -t, --to NEW_DATASET
  [-i, --input-format auto|atif|actf|openai-messages|storyline|codex|claude-code]
  [-o, --output-format preserve|storyline]
  [--max-input-bytes BYTES]
```

规则：

- `--from` 与 `--to` 都是必填；`--to` 是 create-only Dataset target。
- 公共契约不根据输入文件名或 default Warehouse 隐式派生 `--to`。
- 文件或目录默认 `--input-format auto`；stdin 必须显式 input format。
- 默认 `--output-format preserve`；对象存储 `--to` 必须使用 `storyline`。
- `codex` 与 `claude-code` 是 decode-only 输入格式。
- canonical Event Store 自动识别，只允许投影成 Storyline，不接受 JSON `--input-format`。
- `--max-input-bytes` 默认每个 Source 256 MiB；stdin 使用相同的总输入上限。
- directory auto 模式可以跳过无法识别的候选文件，但必须在结果中报告 skipped 数量和原因分类。
- 任一必须输入失败时，最终 Dataset 不得存在。

常见场景：

```bash
pchronicle import --from input.json --to ./imported --input-format atif

pchronicle import -f ./corpus -t ./preserved

pchronicle import -f ./corpus -t s3://bucket/normalized \
  --output-format storyline

pchronicle import -f @codex -t ./codex-dataset

cat input.json | pchronicle import -f - -t ./stdin-dataset \
  --input-format openai-messages

pchronicle import -f ./run/events.lance -t ./run/storyline
```

拒绝案例：

- 输出已存在；
- stdin 使用 `--input-format auto`；
- 对象存储使用 `preserve`；
- squash 后出现重复 `document_id` / `session_id`；
- Source 超过输入预算。

## 5.10 `export`

用途：从一个固定 Snapshot 选择完整 trajectory，编码成交换格式。

```text
pchronicle export
  -f, --from DATASET
  -t, --to PATH_URI_OR_STDOUT
  -o, --output-format atif|actf|openai-messages|storyline
  [--source SOURCE_PATH]
  [--run-id ID]
  [--document-id ID]
  [--session-id ID]
  [--where EXPRESSION]
  [--strict]
  [--overwrite]
  [--max-trajectories N]
  [--max-output-bytes BYTES]
  [--timeout 30s]
  [--max-files N] [--max-entries N]
```

规则：

- `--from`、`--to` 和 `--output-format` 都是必填。
- output format 不得为 decode-only 的 `codex` / `claude-code`。
- identity filters 为 AND；`--source` 用于消除 Source-local ID 歧义。
- `--where` 只针对 `trajectories` view，并与 identity filters 做 AND。
- `--strict` 要求保留原始 exchange document；无法无损转换时整个命令失败。
- `--to -` 直接写 stdout，不再要求 `--stream`。
- 文件和对象输出默认 create-only；`--overwrite` 才允许原子替换。
- 所有 trajectory 必须在写出前通过数量、字节和转换检查，不得产生部分 exchange document。

常见场景：

```bash
pchronicle export --from ./imported \
  --to restored.json --output-format atif

pchronicle export -f ./imported -t - -o storyline

pchronicle export -f ./imported \
  --source source.json --session-id session-42 \
  -t one.json -o actf --strict

pchronicle export -f @prod \
  -t s3://bucket/exports/result.json -o storyline
```

## 5.11 `agent`

用途：启动 Codex 或 Claude，注入临时的只读 Dataset 分析上下文；不是权限沙箱。

```text
pchronicle agent <codex|claude>
  [DATASET]
  [--ask QUESTION | --ask-file FILE_OR_STDIN]
  [--no-overview]
  [--dry-run]
```

规则：

- interactive launch 要求 stdin/stdout 均为 TTY；`--dry-run` 不要求 TTY。
- `--ask` 与 `--ask-file` 互斥；输入上限 16 KiB UTF-8，trim 后不得为空。
- 涉及 secret 或不希望出现在 process list 的问题 SHOULD 使用 `--ask-file`。
- 默认 bootstrap 为 bounded `status` + `analysis overview`，然后提问或回答。
- `--no-overview` 只跳过 generic overview，不跳过 status。
- `--dry-run` 输出版本化 JSON，问题正文必须 redacted；不得检查 Agent 安装/认证、读取 Dataset 或创建临时注入。
- 注入是行为引导，不改变 Codex/Claude 原有 filesystem、network 或 tool permission。
- child 的非零退出必须原样映射为 runtime failure，并在 stderr 标明 target exit status。

常见场景：

```bash
pchronicle agent codex ./dataset
pchronicle agent claude @evals --ask '比较模型延迟'
printf '%s' '分析失败最多的工具' | \
  pchronicle agent codex ./dataset --ask-file - --no-overview
pchronicle agent codex ./dataset --dry-run
```

## 5.12 `serve`

用途：在一个前台生命周期中组合 Warehouse、Control、Gateway 和 projection supervisor。

```text
pchronicle serve
  [--listen LOOPBACK_ADDR]
  [--control LOOPBACK_ADDR]
  [--open]
  [--gateway-config FILE]
  [--gateway-dataset NAME]
  [--gateway-state DIRECTORY]
  [--gateway-stream-markdown]
  [--gateway-debug]
  <[NAME=]DATASET> ...
```

规则：

- 必须提供至少一个 Dataset 位置参数；serve 不读取 default Dataset。
- 未显式选择 `--listen`、`--control` 或 Gateway 时，默认启动 Warehouse 于 `127.0.0.1:0`。
- 显式 `--control` 或 Gateway 且未传 `--listen` 时，不隐式启动 Warehouse。
- 所有未认证 listener 必须是 loopback；非 loopback 在 bind 前拒绝。
- `--open` 要求 `--listen`，且只在 readiness 成功后打开浏览器。
- `--control` 要求存在名为 `default` 的 mount。
- `--gateway-config` 指向完整 Gateway TOML；不在 pChronicle flags 中复制 route、credential 或 network policy。
- 多 Dataset 且无 default 时，Gateway 必须显式 `--gateway-dataset`。
- 对象存储 Gateway capture 必须显式提供本地 `--gateway-state`。
- stdout 必须先 flush 且只输出一行版本化 readiness JSON，然后进程进入服务循环。
- endpoint、projection diagnostic 和请求诊断写 stderr。
- SIGINT/SIGTERM 应执行 bounded graceful shutdown；任一必要组件意外退出则整个 serve 非零退出。

常见场景：

```bash
# 只读 Warehouse，随机 loopback 端口
pchronicle serve ./trajectory-data

# 固定 Warehouse 地址
pchronicle serve --listen 127.0.0.1:8080 --open ./trajectory-data

# Control only
pchronicle serve --control 127.0.0.1:0 default=./trajectory-data

# 多 Dataset Warehouse
pchronicle serve \
  --listen 127.0.0.1:8080 \
  live=./live \
  archive=@archive

# Warehouse + Gateway capture
pchronicle serve \
  --listen 127.0.0.1:8080 \
  --gateway-config gateway.toml \
  --gateway-dataset evals \
  --gateway-stream-markdown \
  evals=./trajectory-data
```

拒绝案例：

- 没有提供 Dataset；
- 非 loopback listener；
- `--open` 没有 `--listen`；
- 对象存储 Gateway capture 没有 `--gateway-state`；
- multi-Dataset Gateway capture 没有可确定的 capture Dataset。

## 5.13 隐藏开发命令 `dev echo`

```text
pchronicle dev echo
  [--listen LOOPBACK_ADDR]
  [--encoding plain|base64]
```

- 只用于 Gateway contract/integration test。
- MUST 只绑定 loopback。
- 不出现在默认帮助、onboard 的公共命令树或产品 capability 列表中。
- MAY 在未来拆成独立 test binary，不提供公共兼容保证。

## 6. 配置与优先级

### 6.1 用户配置路径

```text
-c, --config FILE
> PCHRONICLE_CONFIG
> $XDG_CONFIG_HOME/pchronicle/config.toml
> ~/.config/pchronicle/config.toml
```

相对 `-c, --config` 路径从调用方 cwd 解析。用户配置文件必须拒绝未知字段，最大 1 MiB，并原子写入。
用户配置至少支持一个本地 default Warehouse 和多个 Dataset alias：

```toml
default_warehouse = "/absolute/path/to/local-warehouse"

[aliases]
local = "/absolute/path/to/local-warehouse"
prod = "s3://bucket/evals"
```

Alias value 保存归一化 Dataset URI。相对本地路径在 `alias add/set-url` 时按调用方 cwd 解析并保存为
absolute path，避免以后从不同 cwd 调用时改变含义。配置文件不得保存对象存储 credential。

### 6.2 Dataset 优先级

```text
命令显式 DATASET 位置参数
> 用户配置中的 default Warehouse
> error: not_found
```

`serve`、`import --to` 和 `export --from` 不使用这条 fallback。`@NAME` 在 default fallback 之前解析，但它本身是
显式 Dataset 参数；alias 不存在返回 `not_found`，不得回退为名为 `@NAME` 的本地路径。

### 6.3 环境变量

环境变量只用于用户配置定位、对象存储 provider chain、Agent 启动上下文和标准终端行为。
每个受支持变量必须在 `--help` 或 reference 中列出；不得存在只有读源码才能发现的行为开关。

## 7. Compatibility boundary

公共帮助、onboard、示例、测试与用户文档只使用本文定义的 canonical syntax。实现中临时保留的
隐藏解析入口不属于公共契约，不在文档中枚举，也不得影响 canonical 参数的错误、输出和退出码。
删除隐藏兼容入口时应在 release note 中说明。

## 8. 帮助文本规范

每层 `--help` 必须包含：

1. 一句结果导向的用途说明；
2. 完整 Usage；
3. 参数默认值、单位、互斥和 requires；
4. stdout/stderr 说明；
5. 至少三个 examples：默认、显式 Dataset、自动化；
6. 对会写数据的命令明确 create-only/overwrite；
7. 对 Source-local ID 明确作用域；
8. 指向 `pchronicle onboard <section>` 的学习入口。

帮助和错误必须使用统一术语大小写：Dataset、Source、Snapshot、Warehouse、Storyline、Gateway、Control。
参数占位符统一大写；接受路径、URI 或 alias 的参数使用 `DATASET`，只接受已归一化 URI 的输出字段使用
`dataset_uri`，相对 Source 使用 `SOURCE_PATH`。

## 9. 验收矩阵

### 9.1 Parser contract

- 每个 canonical command 的 `--help` 与 Usage snapshot。
- 全局参数在命令前后等价。
- Dataset-primary 命令使用位置参数，且 `query` 的 Dataset 与 SQL 不发生位置歧义。
- 所有互斥、requires 和 exactly-one group 在执行 I/O 前失败。
- deprecated 语法只发一次 warning，行为与 canonical 语法相同。
- Clap usage error 固定退出 2，stdout 为空。

### 9.2 Output contract

- TTY/pipe 下 `auto` 的格式矩阵。
- JSON/JSONL schema version、字段类型和 newline。
- stdout 只含结果，stderr 只含 metadata/error。
- 改变 `--log-level` 不改变 stdout。
- 超限、timeout 和编码失败不产生部分 stdout/目标文件。
- `serve` readiness 恰好一行并在进入循环前 flush。

### 9.3 Filesystem/object contract

- create-only 目标冲突退出 4。
- `--overwrite` 是原子替换。
- alias add/set-url/rename/remove 原子更新配置，任何失败保留旧 alias 集合。
- `@NAME`、裸相对路径和 `@NAME/SUFFIX` 的解析不依赖当前目录中是否存在同名项。
- import 失败不留下最终 Dataset。
- local、`file://`、S3-compatible 与测试 object store 行为一致。
- URI credential/query/fragment 被拒绝且错误不泄漏 secret。

### 9.4 Semantic contract

- `status` degraded/partial 不伪装 exact。
- `find` 保留 `source_path`，无匹配与 Source 不存在区分。
- query 只读、单 statement、结果有界。
- export 始终输出完整 trajectory；strict conversion 失败不部分写出。
- Agent dry-run 不读 Dataset、不启动 child、不创建临时注入。
- serve 的 component selection、readiness 和 loopback restriction 全矩阵测试。

## 10. 明确不做的事情

- 不把所有限制都提升为全局 flag；参数只出现在真正消费它的命令。
- 不让 `auto` 成为自动化兼容契约。
- 不允许隐式覆盖输出。
- 不用 exit code 表示 Dataset 的业务健康；`--errors report` 的健康状态在结果中表达。
- 不把 Agent 注入描述为权限隔离。
- 不把 Echo、benchmark、repair 或内部 maintenance 暴露为稳定顶层产品命令。
- 不在本提案中增加新的 analysis 类型或 SQL relation。
