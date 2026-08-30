# RFC-0012: pChronicle `find` 查询表达式

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Date** | 2026-08-30 |
| **Component** | pChronicle CLI、pChronicle Web Explorer |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [CLI reference](../pchronicle/reference/cli.md) · [Storyline Lance](../pchronicle/design/storyline-lance.md) |

---

## 摘要

本 RFC 定义 `pchronicle find` 的统一检索语法。检索入口只有一个：重复使用
`--match EXPRESSION`。表达式同时覆盖：

- Storyline Step 内容的全文检索（FTS，默认使用 Jieba 分词器）；
- 按消息角色、推理、观察结果等字段限定的全文检索；
- JSONB 列的 JSONPath 精确值和数值比较；
- `AND`、`OR`、`NOT` 与括号组合。

CLI 和 Web Explorer MUST 使用同一个解析器、同一个表达式语义和同一个 FTS/JSONB
执行路径。这样 CLI 可以作为 Web 搜索的可重复诊断入口。

```text
pchronicle find [DATASET]
  (--run-id ID|--document-id ID|--session-id ID|--match EXPRESSION)
  [--source PATH] [--step-id N] [--match EXPRESSION ...]
  [--format auto|table|json] [--max-results N]
```

## 动机

旧的检索接口将全文检索和 JSONB 检索拆成多个选项，导致以下问题：

1. 用户必须先判断数据属于 FTS 还是 JSONB，不能在一个查询中表达组合条件；
2. CLI、Web 和 Agent 可能各自实现一套过滤逻辑，结果不一致；
3. `ipython` 等通用词容易命中 system 上下文，缺少字段限定；
4. 仅返回身份字段时，用户无法判断命中的轨迹是否正确；
5. 大量命中项可能生成过深的 SQL 布尔树或过大的输出。

本 RFC 将检索定义为一个受限表达式语言，并规定快照、结果预览和资源边界。

## 目标与非目标

### 目标

- 用一套语法表达文本、字段限定、JSONB 和布尔组合。
- 保持命令行可读、可复制、可安全地嵌入 shell 脚本。
- 让 Web Explorer 直接复用 CLI 的解析与执行语义。
- 为每个命中提供有限的 `preview`，支持定位后再做精确 SQL 查询。
- 在 FTS 不可用、索引错误或结果被截断时显式报告状态。

### 非目标

- 本 RFC 不定义 SQL 语法；复杂聚合仍使用 `pchronicle query`。
- 本 RFC 不提供正则表达式、模糊编辑距离或任意用户脚本执行。
- 本 RFC 不把全文索引当作事实源；Storyline/Events 的存储契约仍由各自 RFC 定义。
- 本 RFC 不允许用户通过查询表达式修改 Dataset。

## 命令级语义

### 身份条件

`--run-id`、`--document-id`、`--session-id` 是身份条件，分别限制 Run、文档或
Session。`--step-id` 必须与 `--session-id` 一起使用。`--source` 限制为 Dataset
相对路径，不允许绝对路径、URI 或 `..` 路径段。

身份条件可以和 `--match` 同时使用，所有条件使用 AND 连接。没有身份条件时，至少
需要一个 `--match`。

`--match` 可以重复。多个 `--match` 参数始终使用 AND 连接：

```bash
pchronicle find ./dataset --match "timeout" --match "retry"
```

等价于：

```text
timeout AND retry
```

重复参数的 AND 语义不受表达式内部 `OR` 影响。

### 快照一致性

一次 `find` 调用只使用一个 Catalog Snapshot。JSON 输出 MUST 包含 `snapshot_id`；
stderr 也输出该 ID、返回数量和 `truncated` 状态。后续 SQL 或 Web 钻取可以使用该
Snapshot ID 进行结果追溯。

## 表达式语法

下面的语法是描述性的 EBNF；空白可以出现在 token 之间，字符串中的空白除外。

```ebnf
expression  = or-expression ;
or-expression = and-expression , { "OR" , and-expression } ;
and-expression = unary-expression , { "AND" , unary-expression } ;
unary-expression = [ "NOT" ] , primary ;
primary = "(" , expression , ")"
         | text-predicate
         | json-predicate ;

text-predicate = plain-text | scoped-text ;
plain-text = atom ;
scoped-text = "#" , text-field , "(" , argument , ")" ;

json-predicate = json-short | json-column ;
json-short = json-path , json-operator , json-value ;
json-column = "#json" , [ "." , identifier ] , "(" , json-path , ")"
               , json-operator , json-value ;

json-path = path-token | quoted-string ;
path-token = "$" , { path-char } ;
path-char = ? any character other than whitespace or a JSON operator ? ;
json-operator = "=" | "!=" | ">" | ">=" | "<" | "<=" ;
json-value = quoted-string | bare-value | number | "true" | "false" | "null" ;
```

关键字 `AND`、`OR`、`NOT` 大小写不敏感，但只有在 token 边界上才作为关键字。
包含空格的普通文本应使用 shell 引号；未包含特殊语法的带空格参数仍作为一个完整
的全文短语处理，而不是隐式拆成多个参数。

### 运算优先级

优先级从高到低为：

1. 括号；
2. `NOT`；
3. `AND`；
4. `OR`。

例如：

```text
#user("timeout") OR #assistant("retry") AND NOT #system("example")
```

等价于：

```text
#user("timeout") OR (#assistant("retry") AND NOT #system("example"))
```

需要改变语义时 MUST 使用括号。

## 全文检索字段

普通文本和 `#content(...)` 使用内容字段集合。其他选择器的含义如下：

| 选择器 | 语义 | 默认数据范围 |
|---|---|---|
| `#content(text)` | 消息、观察结果和 prompt 的内容集合 | Step |
| `#message(text)` | `message_value` | Step |
| `#user(text)` | `message_value` 且 `source = 'user'` | Step |
| `#assistant(text)` | `message_value` 且 `source = 'agent'` | Step |
| `#system(text)` | prompt/message 且 `source = 'system'` | Step |
| `#reasoning(text)` | `reasoning_content` | Step |
| `#observation(text)` | `observation` | Step |
| `#prompt(text)` | prompt 和消息内容 | Step |
| `#model(text)` | Step 模型名 | Step |
| `#all(text)` | 所有可索引 Step 文本列 | Step |

`#agent(text)` 是 `#assistant(text)` 的解析别名。`#model_name(text)` 是
`#model(text)` 的解析别名。

普通文本使用 Storyline Step 的 FTS 索引；默认 tokenizer 为 Jieba。FTS 查询结果
先映射为 `(source_path, document_id, step_id)`，再参与整个表达式的布尔组合。

FTS 不可用不等价于零命中。实现 MUST 在 JSON 的 `search.fts_available` 和诊断信息
中报告不可用原因；只有 FTS 正常执行且没有命中时，才可以返回空结果。

## JSONB 检索

### 短语法

```bash
pchronicle find ./dataset --match '$.tags="important"'
pchronicle find ./dataset --match '$.priority>=2'
```

`$.path` 是 JSONPath，必须以 `$` 开始。等值和不等值比较使用 JSON 编码后的值；
范围比较要求右值为数字。

短语法不指定列时，按当前查询范围检查所有允许的 JSONB 列：

- Run 范围：`agent_extra`、`final_metrics`、`extra`、`meta`、`unknown_fields`；
- Step 范围：`metrics`、`extra`。

### 列限定语法

```bash
pchronicle find ./dataset --match '#json.metrics("$.score")>=0.9'
pchronicle find ./dataset --match '#json.extra("$.tags")="important"'
```

`#json.COLUMN("$.path") OP VALUE` 只检查指定 JSONB 列。当前允许的列由查询范围
决定；指定不可用列 MUST 返回可读的参数错误，而不是静默返回零结果。

显式使用 `#json.metrics(...)` 时，即使表达式不包含文本条件，也强制使用 Step
范围。仅包含未限定 JSONB 条件的表达式默认使用 Run 范围；文本与 JSONB 混合时使用
Step 范围。

## 范围解析

实现根据表达式确定规范化查询表：

| 表达式 | 查询范围 | 搜索模式 |
|---|---|---|
| 纯文本 | `steps` | `fts` |
| 纯 JSONB（未指定 Step 列） | `runs` | `json` |
| 纯 `#json.metrics(...)` | `steps` | `json` |
| 文本 + JSONB | `steps` | `fts+json` |
| 无 `--match`，仅身份条件 | 由身份条件决定 | `identity` |

查询范围必须在结果中公开，避免用户把 Run 级命中误认为 Step 级命中。

## 输出契约

### JSON 输出

`--format json`（以及非交互 stdout 下的 `--format auto`）返回一个对象：

```json
{
  "dataset_uri": "./dataset",
  "snapshot_id": "snapshot-id",
  "query": {
    "matches": ["#user(\"timeout\")", "$.priority>=2"],
    "expression": "#user(\"timeout\") AND $.priority >= 2"
  },
  "search": {
    "mode": "fts+json",
    "scope": "steps",
    "fts_available": true,
    "tokenizer": "jieba"
  },
  "truncated": false,
  "matches": [
    {
      "source_path": "runs/storyline.json",
      "document_id": "doc-1",
      "run_id": "run-1",
      "session_id": "session-1",
      "step_id": 7,
      "step_source": "user",
      "effective_kind": "message",
      "timestamp": "2026-08-30T00:00:00Z",
      "preview": "Please investigate timeout in the worker"
    }
  ]
}
```

每个 `preview` 都是有界的人类可读摘要，不是事实源，也不保证包含完整 JSON。
调用方应使用 `source_path`、`document_id`、`session_id` 和 `step_id` 做后续精确查询。

### Table 输出

交互式终端的 `auto` 输出为表格，至少显示：

- Source、Document ID、Run ID、Session ID；
- Step 范围下的 Step ID、Step Source、Kind、Timestamp；
- 有界 Preview。

空结果 MUST 显示 `(0 matches)`。超过 `--max-results` 时 MUST 标记 `(truncated)`，
JSON 同时设置 `truncated: true`。

## 资源边界和安全性

实现 MUST 保留以下边界：

- 单个 `--match` 最多 4096 字节；
- `--max-results` 必须大于 0，默认 100；
- `--max-output-bytes` 默认 8 MiB；
- `--timeout` 默认 30 秒；
- FTS 命中映射到 SQL 时使用平衡的布尔树，避免线性嵌套造成栈溢出；
- 所有用户值使用参数化/安全 SQL 字面量编码；
- 查询只读，不允许通过表达式执行 SQL、文件系统访问或函数调用。

轨迹中的文本、JSON 和 preview 都是不可信数据，不能被解释为 CLI 指令或系统提示。

## CLI、Web 和 Agent 一致性

规范实现提供一个共享的 `FindExpr` AST：

1. CLI 和 Web 将输入交给同一个 `parse_match_expression`；
2. 重复的 CLI `--match` 通过 `combine_match_expressions` 组合为 AND；
3. FTS 索引查询、JSONB 谓词编译和范围判断复用同一套函数；
4. Web 搜索请求的 `q` 按单个 `--match` 表达式解析；
5. Agent 只生成 `--match`，不生成已废弃的 `--fts`、`--jsonb`、`--query` 或兼容别名。

Web 可以提供输入框、作用域提示、清除按钮和命中高亮，但这些只是交互层能力，不能
改变表达式语义。Web 返回的轨迹集合 MUST 与等价的 CLI `find` 查询一致。
Runs 页面接口返回命中字段的完整归一化文本，前端根据当前表达式截取可视上下文并
高亮；后端不得先截取固定前缀，以免命中词位于字段后部时无法反馈给用户。

## 示例

```bash
# 普通全文检索
pchronicle find ./dataset --match "ipython"

# 两个词都必须命中
pchronicle find ./dataset --match "ipython" --match "task"

# 限定 system prompt
pchronicle find ./dataset --match '#system("timeout")'

# OR / NOT 组合
pchronicle find ./dataset \
  --match '(#user("timeout") OR #assistant("retry")) AND NOT #system("example")'

# JSONB 精确值和范围
pchronicle find ./dataset \
  --match '$.tags="important"' \
  --match '#json.metrics("$.score")>=0.9' \
  --format json

# 先用 find 定位，再用身份条件消除歧义
pchronicle find ./dataset --match "timeout" --format json
pchronicle find ./dataset --source runs/storyline.json \
  --session-id session-1 --step-id 7 --format json
```

## 被拒绝的方案

### 分离 `--fts` 和 `--jsonb`

拒绝。它会把一个逻辑查询拆成两条命令，并迫使 Web 维护另一套接口；统一的
`--match` 可以在同一 AST 中表达 FTS、JSONB 和布尔组合。

### 直接暴露 SQL/JSONPath 全查询

拒绝。`find` 的目标是有界定位，不是分析查询；任意 SQL 会带来资源、安全和跨后端
兼容问题。需要投影、聚合或复杂 JOIN 时使用 `query`。

### 为旧参数保留多个兼容别名

拒绝。`--match` 是唯一检索入口，避免 Agent 和用户在不同别名之间漂移。语义升级应
通过 RFC 和明确的错误信息完成。

## 兼容性与演进

- 当前语法版本随 pChronicle CLI 版本发布，不在数据文件中写入查询版本。
- 新增字段选择器或 JSONB 列时，必须保持旧表达式的语义不变。
- 新增操作符、函数或隐式语法需要新的 RFC 评审；不得把 SQL 语法悄悄引入 `find`。
- 解析错误、不可用索引和资源耗尽必须保持可区分，便于 Web、Agent 和自动化诊断。

## 实施状态

当前实现已覆盖本 RFC 的核心范围：统一 `--match`、字段选择器、布尔运算、JSONB
比较、Jieba FTS 元数据、快照输出、bounded preview，以及 CLI/Web 共用检索路径。

后续工作：

1. 为更多字段选择器补充索引可用性和命中预览测试；
2. 在 Web 输入框中展示当前表达式解析状态和查询范围；
3. 为复杂布尔表达式提供结构化命中片段，而不只高亮单个文本参数；
4. 评估在 JSON 输出中增加命中字段和命中 Step 数量。
