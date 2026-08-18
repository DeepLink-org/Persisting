# pChronicle

pChronicle 是 Persisting 的结构化轨迹与 Dataset 数据层。它负责轨迹领域模型、磁盘格式、
Lance 持久化、数据源发现、DataFusion 查询、格式交换和可重建投影；其他 crate 可以生产或
消费轨迹，但不应再定义第二套存储格式或轨迹协议 DTO。

## 权威模型与数据流

Canonical Event 与 Storyline 分别在事实层和交换/分析层保持权威，关系是单向投影：

```text
events.lance ──单向投影──► StorylineDocument ──► Storyline 三表 Lance
                                  ▲  │
                                  │  ├──◄──► AgenticMD
                                  │  ├──◄──► ATIF v1.7
                                  │  ├──◄──► OpenAI Msg
                                  │  └──◄──► ACTF
                                  └────────── 格式转换枢纽
```

- `events.lance` 是 append-only 的运行时事实源；Storyline 不能无损反建原始事件事实。
- `StorylineDocument` 是与 ATIF v1.7 对齐的权威轨迹模型，也是所有外围格式转换的唯一枢纽。
- Storyline Lance 以 `runs`、`steps`、`tool_calls` 和可选 `objects` 表保存该模型。
- AgenticMD 只是 Storyline 的人类可读 Markdown 编码，不拥有独立领域模型。

## 六种磁盘格式

| `DocumentFormat` | 磁盘表示 | DataFusion 表 | 与 Storyline 的关系 |
|---|---|---|---|
| `CanonicalEvent` | `events.lance` manifest 与 segments | `events` | 单向投影到 Storyline |
| `Storyline` | Storyline 三表 Lance | `runs`、`steps`、`tool_calls` | 权威二进制表示 |
| `AgenticMd` | Markdown 文件 | `runs`、`steps`、`tool_calls` | 可读编码，可双向转换 |
| `Atif` | ATIF JSON/JSONL/NDJSON | `runs`、`steps`、`tool_calls` | ATIF v1.7 对齐，可双向转换 |
| `OpenaiMsg` | OpenAI message corpus JSON | `runs`、`steps`、`tool_calls` | 通过分层 residual 无损往返 |
| `Actf` | ACTF JSON | `runs`、`steps`、`tool_calls` | 通过分层 residual 无损往返 |

统一读取入口是 `document::open_document`；返回的 `DocumentSource` 隐藏具体 provider，
并提供有预算上限的物化、逐条 Storyline 回调和 DataFusion 注册。写入仍使用
`storage` 中按物理格式区分的类型化 API，不使用包含互斥字段的通用写入对象。

## 无损边界

以下路径保证 JSON 数据模型级无损，不保证空白、缩进或对象键顺序逐字节一致：

```text
ATIF       → Storyline Lance → ATIF
ACTF       → Storyline Lance → ACTF
OpenAI Msg → Storyline Lance → OpenAI Msg
```

保真内容包括 ATIF 的 missing/null/value 三态、嵌套 subagent 顺序、`trajectory_id` 与
run-scoped `session_id` 的独立身份、RFC3339 原始偏移与亚毫秒精度，以及 ACTF/OpenAI 的
未知字段、数组顺序、attempt 分组和多 session 关系。外围格式无法映射到正式 Storyline
字段的内容保存在对应语义层级的受控 residual 中；不会保存完整原始对象副本。跨格式转换
只保证目标格式能够表达的语义。Canonical Event → Storyline 是有意的有损规范化投影，
不属于上述无损承诺。

ATIF 的顶层单对象/数组形态与 root 顺序作为格式无关的 Storyline 集合语义随 Lance
持久化，不依赖进程内 sidecar；Lance 内部另用 `storage_ordinal` 维护全局稳定读取顺序，
不会把多次增量写入都退化到 document id 排序。无法用任何 Storyline 表达的空 ATIF 数组
或空 OpenAI 信封会 fail closed，而不是接受后在导出时静默丢失容器字段。

## DataFusion 能力

能力由实际打开的 `DocumentSource::capabilities()` 报告，不按格式名称推断：

| 格式 | projection | filter | limit | scalar index | streaming decode | snapshot |
|---|---:|---|---:|---:|---:|---:|
| Canonical Event | 是 | exact | 是 | 是 | 是 | 是 |
| Storyline Lance | 是 | expression-dependent | 是 | 是 | 否 | 是 |
| ATIF | 是 | inexact | 是 | 否 | 是 | 否 |
| OpenAI Msg | 是 | unsupported | 是 | 否 | 否 | 否 |
| ACTF | 是 | unsupported | 是 | 否 | 否 | 否 |
| AgenticMD | 是 | unsupported | 否 | 否 | 否 | 否 |

Canonical Event 保留 Lance projection/filter/limit pushdown、scalar index、pinned manifest
和 append-order scan。Storyline Lance 保留三表 provider 与 late content materialization。
文本格式不虚报 Lance 能力。物化或 fallback 超过行/字节预算时返回
操作失败，不会静默截断结果；HTTP/CLI 查询输出预算由边界拥有的显式
`LimitExceeded` 结果映射为 `resource_exhausted`。

## 公共 API

默认功能面只通过四个模块组织：

- `model`：Storyline、Canonical Event 与 LLM payload 的权威运行时类型；
- `document`：`DocumentFormat`、以 Storyline 为输入输出的高层 codec 和统一读取入口；
- `storage`：路径、Catalog、Lance store、append、投影与 revision；
- `query`：QueryEngine 和能力/快照类型。

外围 wire DTO、低层格式 parser、AgenticMD AST、Arrow codec、DataFusion provider、
provider options、manifest、锁和底层投影辅助实现均保持私有或 crate 内可见。
`search` 是独立 feature，本轮公共门面收敛不改变它。

错误门面保持轻量：crate 根以及 `document`、`storage`、`query` 模块公开的
`Result<T>` 都精确等同于 `anyhow::Result<T>`。操作失败直接使用 `?` 保留具体 source；
lookup 用 `Result<Option<T>>` 表达缺失；parser/validator 只通过
`document::{InputIssue, InputIssueKind, InputResult}` 表达可安全反馈的输入问题；append、
projection 等调用方需要分支的状态使用所属模块的局部 Outcome。公共 API 不提供全局
`Error`、错误码、分类器或传播上下文协议。

## 组件边界

| 组件 | 职责 |
|---|---|
| `persisting-pchronicle` | 模型、格式、Catalog、Lance 存储、读取器和查询引擎 |
| `persisting-pchronicle-cli` | `pchronicle` 命令、loopback 只读 API 与 Web 静态资源 |
| `pchronicle-web` | Dioxus 浏览前端 |

常用命令：

```bash
pchronicle ls ./dataset
pchronicle status ./dataset
pchronicle query ./dataset "SELECT * FROM dataset.runs"
pchronicle import --from input.json --output ./imported --format atif
pchronicle export --from ./imported --output output.json --format storyline
pchronicle project build --from ./run/events.lance --output ./run/storyline
pchronicle project verify --from ./run/storyline --source ./run/events.lance
pchronicle project sync --from ./run/storyline --source ./run/events.lance
```

参见 [`pchronicle` 命令参考](../../docs/src/design/cli-pchronicle.md)和
[RFC-0003](../../docs/src/rfcs/0003-pchronicle-ownership.md)。
