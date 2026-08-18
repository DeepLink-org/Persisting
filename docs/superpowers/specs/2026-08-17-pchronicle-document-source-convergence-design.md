# pChronicle 文档源与查询面收敛设计

状态：已实施（2026-08-18）；最终验收见本轮提交记录

日期：2026-08-17

范围：pChronicle 的 Storyline、Canonical Event、AgenticMD、ATIF、OpenAI Msg、ACTF、DataFusion 查询入口与公共 API

## 1. 摘要

pChronicle 将 Storyline 定位为以 ATIF v1.7 语义为基准的权威轨迹模型。Storyline 三表 Lance 是该模型的二进制磁盘格式，AgenticMD 是该模型的人类可读 Markdown 编码。ATIF、OpenAI Msg 和 ACTF 都通过 Storyline 互转，不再借助隐藏的原始对象副本或进程内 provenance sidecar。

Canonical Event 作为第六种磁盘类型纳入统一的文档源发现和 DataFusion 打开入口，但保持独立事实语义。它通过投影产生 Storyline，不能由 Storyline 无损反向重建。

本设计同时完成以下清理：

1. 消除转换层静默丢错和静默丢字段；
2. 用一个磁盘格式枚举替代包含不可能操作的 `ChronicleFormat`；
3. 让 Storyline 自身承载无损转换所需的权威信息；
4. 删除已被通用文件源取代的旧 ATIF DataSource；
5. 收敛 pChronicle 公共门面；
6. 删除 pChronicle 与 `persisting-events` 重复的轨迹协议 DTO；
7. 将现有 DataFusion 优化保留并延伸到统一的文档源能力模型。

## 2. 范围与非目标

### 2.1 本轮范围

- Storyline 与 ATIF 的字段对齐和无损边界；
- AgenticMD 绑定 Storyline；
- 六种磁盘格式的统一识别与读取入口；
- Canonical Event 与 Storyline 的查询面区分；
- DataFusion provider 注册、能力报告和 QueryEngine 构造入口；
- 严格转换错误；
- ATIF DataSource、重复轨迹 DTO 和过宽公共 API 的删除；
- pChronicle CLI 与 Gateway 的必要迁移。

### 2.2 非目标

- 不实施 canonical v2；
- 不修改 Canonical Event 的身份编码、Arrow schema 或 manifest 版本；
- 不改变 Canonical Event 的 append-only、writer fence 和 segment publication 语义；
- 不进入 Search、TTAS、Queue/Sampler 或 standalone dlcapt 内部实现；
- 不完成全 crate 的 typed-error 迁移；
- 不承诺把 Storyline 无损反向转换成原始 Canonical Event；
- 不为 JSON 或 Markdown 伪造 Lance scalar-index 能力。

## 3. 权威模型与磁盘格式

### 3.1 模型关系

```text
events.lance ──投影──► StorylineDocument

ATIF JSON       ◄──► StorylineDocument ◄──► Storyline 三表 Lance
OpenAI Msg JSON ◄──► StorylineDocument ◄──► AgenticMD Markdown
ACTF JSON       ◄──► StorylineDocument
```

`events.lance` 是运行时事实源。Storyline 是格式转换和规范化查询域中的权威模型。两者的“权威”作用于不同边界，不互相替代。

### 3.2 唯一磁盘格式枚举

```rust
pub enum DocumentFormat {
    CanonicalEvent,
    Storyline,
    AgenticMd,
    Atif,
    OpenaiMsg,
    Actf,
}
```

各 variant 的精确定义：

| Variant | 磁盘表示 | 逻辑查询表 |
|---|---|---|
| `CanonicalEvent` | `events.lance` manifest 与 segments | `events` |
| `Storyline` | `runs`、`steps`、`tool_calls`、`objects` Lance | `runs`、`steps`、`tool_calls` |
| `AgenticMd` | Storyline Markdown 文件 | `runs`、`steps`、`tool_calls` |
| `Atif` | ATIF JSON、JSONL 或 NDJSON | `runs`、`steps`、`tool_calls` |
| `OpenaiMsg` | OpenAI message JSON | `runs`、`steps`、`tool_calls` |
| `Actf` | ACTF JSON | `runs`、`steps`、`tool_calls` |

`DocumentFormat` 只描述磁盘表示，不承诺每个 variant 支持相同的写入输入类型。

## 4. Storyline 与 ATIF 对齐

### 4.1 正式字段原则

Storyline 的正式字段以 ATIF Trajectory、Step、ToolCall 和 Observation 为基准：

- `StorylineDocument` 对应 ATIF Trajectory；
- `StorylineTurn` 对应 ATIF Step；
- `StorylineToolCall` 对应 ATIF ToolCall；
- `agent`、`notes`、`final_metrics`、`continued_trajectory_ref`、`observation`、`metrics` 与 ATIF 保持同义。

Storyline 只保留以下已接受增量：

- required `agent.id`；
- parent/children 外链；
- `kind`；
- `latency_ms` 与 `ttft_ms`；
- tool call `duration_ms`；
- pChronicle 现有存储所需的 run/session 对应关系；
- `attempt_id`（可选）：Attempt 作用域身份。外部格式导入时为 null；该字段为
  canonical v2 的运行时投影预留，不参与 ATIF 对齐字段。

`session_id` 维持当前会话身份语义（≈ ATIF Trajectory 的 `session_id`，子轨迹走
parent/children 外链），不表示 pChronicle Run；Run 关联使用 `run_id`。

**ATIF 基线版本策略**：本设计将 Storyline 正式字段的 ATIF 语义基线显式声明为
v1.7。ATIF 发布新版本时，必须先做字段 diff 评审，再更新本文声明的基线与增量
清单，并同步扩展 §14 的无损往返测试；Storyline 字段集自身携带模型版本信息，
禁止在未评审的情况下隐式跟随外部规范演进。

### 4.2 必须补齐的 ATIF 语义

- `StorylineToolCall` 增加 ATIF inline `result`；
- 对 ATIF 要求保真的值使用能区分"字段缺失""显式 null""实际值"的表示；
- ATIF observation 解析失败必须返回错误，不能删除 observation；
- ATIF schema/version 信息必须能经过 Storyline 三表 split/reconstruct 保留；
- ATIF adapter 必须是近似结构映射，不保存 `_pchronicle_atif_tool_call` 等原始对象副本。

**三态表示的类型设计**：缺失/显式 null/实际值必须使用一个命名的公共三态类型
（如 `pub enum Field<T> { Missing, Null, Value(T) }`），禁止 `Option<Option<T>>`
等隐式嵌套。serde 映射固定为：缺失 = 字段不出现在 JSON；显式 null = 键存在且为
null；值 = 正常序列化。Arrow 三表落盘用可空列加显式存在性表达，split/reconstruct
与无损往返测试必须逐字段覆盖全部三态。

`AtifTrajectory`、`AtifStep` 等可以保留为私有 wire DTO，但不再与 Storyline 一起构成两套公开领域模型。

## 5. 无损往返与 residual extensions

### 5.1 权威性规则

Storyline 自身必须包含目标转换器所需的全部信息。禁止使用以下机制：

- 进程内 provenance sidecar；
- 绑定单一来源格式的 provenance enum；
- `_pchronicle_*` 私有键；
- 完整原始 document、record、step 或 tool-call 对象副本。

### 5.2 residual 规则

不能映射到 ATIF/Storyline 正式字段的外围格式字段，存入对应语义层级的 `extra`：

- ACTF 文档级剩余字段进入 document `extra`；
- ACTF attempt/step 剩余字段进入对应 Storyline/turn `extra`；
- tool 与 observation 的事件专属字段进入对应对象扩展；
- OpenAI 文件容器、相对路径等文件级信息进入 document `extra`；
- OpenAI ordinal 与未映射 record 字段进入 turn `extra`。

每个 adapter 必须先从源对象移除已经映射的键，再保存 residual。导出时先根据 Storyline 正式字段生成目标对象，再合并无冲突 residual。若 residual 与正式字段映射到同一目标键，正式字段获胜，并必须记录一条结构化诊断（源键、目标键、来源格式），不得静默丢弃——与本设计"消除静默丢字段"的目标一致。

### 5.3 保真边界

以下路径保证 JSON 数据模型级无损：

```text
ATIF       → Storyline Lance → ATIF
ACTF       → Storyline Lance → ACTF
OpenAI Msg → Storyline Lance → OpenAI Msg
```

保留键值、显式 null、未知字段、数组顺序、ACTF attempt 分组和 OpenAI 多 session 关系。不保证空白、缩进和对象键顺序逐字节一致。

跨格式 `F1 → Storyline → F2` 输出 F2 能表达的全部语义。如果 F2 没有相应字段或扩展通道，不承诺再经过 F2 恢复 F1 私有字段。

## 6. AgenticMD 作为 Storyline Markdown

AgenticMD 不再定义独立领域模型。公开语义接口为：

```rust
pub fn parse_agenticmd(input: &str) -> Result<StorylineDocument>;
pub fn encode_agenticmd(story: &StorylineDocument) -> Result<String>;
```

约束：

- frontmatter 映射 `StorylineDocument`；
- 每个正文 block 映射 `StorylineTurn`；
- tool calls、observation、metrics、模型和时延都使用 Storyline 字段；
- 私有 Markdown AST 只负责语法、byte span 和增量编辑；
- 结构化 HTML comment 只能序列化 Storyline 类型；
- 删除 AgenticMD correlation keys 在 `turn.extra` 中的搬运；
- 删除公开 `AgenticmdDocument`、`AgenticmdBlock`、`AgenticmdHeader`。

Gateway 的投影路径统一为：

```text
EventRecord[] → Storyline projection → AgenticMD encoding
```

## 7. 文档源读取与类型化写入

### 7.1 统一读取入口

```rust
pub async fn open_document(
    format: DocumentFormat,
    path: &Path,
) -> Result<DocumentSource>;
```

`DocumentSource` 是公开 struct，其具体 provider enum 保持私有。默认入口使用各 provider
已有的安全默认值；确有高级配置需求的调用者继续使用对应的类型化 provider 构造器，本轮
不设计一个包含大量互斥字段的通用 options 对象。

`DocumentSource` 提供：

```rust
impl DocumentSource {
    pub fn format(&self) -> DocumentFormat;
    /// 物化全部 Storyline。源规模超过配置的行/字节预算时返回
    /// `Error::SourceBudgetExceeded`（fail closed），不做无界分配。
    pub async fn project_storylines(&self) -> Result<Vec<StorylineDocument>>;
    /// 流式投影：逐条回调，内存保持有界。大源必须使用此入口。
    pub async fn for_each_storyline<F>(&self, on_storyline: F) -> Result<()>;
    pub fn register_datafusion(
        &self,
        context: &SessionContext,
    ) -> Result<QueryTables>;
}
```

六种磁盘源都能打开、查询并投影为 Storyline。物化入口与流式入口共用同一
provider 管道；预算耗尽是显式错误，不是静默截断。

### 7.2 类型化写入

写入不使用一个接受所有 enum variant 的通用 `save`：

```rust
RawEventLanceStore::append_events(&[EventRecord]);
StorylineLanceStore::replace_storylines(&[StorylineDocument]);
encode_agenticmd(&StorylineDocument);
write_atif(&[StorylineDocument]);
write_openai_msg(&[StorylineDocument]);
write_actf(&[StorylineDocument]);
```

禁止 `save(DocumentFormat::CanonicalEvent, &[StorylineDocument])`。Storyline 到 EventRecord 的现有合成转换只能保留为明确命名的调试/导出能力，不能写入或宣称重建 canonical facts。

`storyline.json` 不再是一等 `DocumentFormat`。`StorylineDocument` 可以继续使用 serde JSON，供测试、调试和内部传输使用。

## 8. DataFusion 查询设计

### 8.1 统一内部接口

```rust
pub(crate) trait QueryDocumentSource {
    fn format(&self) -> DocumentFormat;
    fn tables(&self) -> QueryTables;
    fn capabilities(&self) -> QueryCapabilities;
    fn register(&self, context: &SessionContext) -> Result<()>;
}
```

```rust
pub struct QueryCapabilities {
    pub projection_pushdown: bool,
    pub filter_pushdown: FilterPushdown,
    pub limit_pushdown: bool,
    pub scalar_indexes: bool,
    pub streaming_decode: bool,
    pub late_content_materialization: bool,
    pub snapshot_consistent: bool,
}
```

```rust
pub enum QueryTables {
    Events,
    Storyline,
}

pub enum FilterPushdown {
    Unsupported,
    Inexact,
    Exact,
    ExpressionDependent,
}
```

`ExpressionDependent` 表示 provider 必须按具体表达式调用
`supports_filters_pushdown` 决定 `Unsupported`、`Inexact` 或 `Exact`，不能把混合能力压成
一个过度承诺的静态值。

能力必须按 provider 的真实实现报告，不能为了接口统一虚报 `Exact` filter pushdown。

### 8.2 各格式能力

#### Canonical Event

- 注册 `events` 表；
- 保留 Lance projection、exact filter pushdown、scalar index、segment union、pinned manifest、append-order range scan；
- 默认不实时注册 `runs/steps/tool_calls`；
- 需要 Storyline 查询面时，优先使用 lineage 新鲜的 Storyline Lance 投影；
- 无投影时只能在已有 row/byte budget 内执行 bounded fallback；预算耗尽必须返回
  `Error::SourceBudgetExceeded`（fail closed），不得静默截断结果集。

#### Storyline Lance

- 注册 `runs/steps/tool_calls`；
- 保留 Lance projection、非内容列 exact filter、scalar index、条件式 limit pushdown；
- 保留 content sidecar late hydration 与 preview 模式；
- 内容列谓词继续按当前安全规则报告 unsupported 或 fail closed。

#### ATIF

- 注册 `runs/steps/tool_calls`；
- 保留文件裁剪、step filter inexact pushdown、字段投影、流式解析、bounded-memory reader 和 cache；
- 不退化现有 ATIF streaming metrics 与测试。

#### OpenAI Msg 与 ACTF

- 注册 `runs/steps/tool_calls`；
- 保留文件级裁剪、解析缓存、投影后的 Arrow batch、文件大小和并发限制；
- 未实现行级安全下推前报告 unsupported。

#### AgenticMD

- 解析为 Storyline 后复用 Storyline Arrow row codec；
- 注册 `runs/steps/tool_calls`；
- 第一版仅报告文件级裁剪与 projection；
- 行级 filter pushdown 初始为 unsupported；
- 不为 DataFusion 创建独立 AgenticMD schema。

### 8.3 QueryEngine 收敛

QueryEngine 公开构造入口收敛为：

```rust
pub async fn ChronicleQueryEngine::open(
    format: DocumentFormat,
    path: impl AsRef<Path>,
    options: ChronicleQueryExecutionOptions,
) -> Result<Self>;
```

删除格式专属的重复 `open_*`、`from_*` 构造器。后端信息改为统一结构：

```rust
pub struct QueryBackendInfo {
    pub format: DocumentFormat,
    pub tables: QueryTables,
    pub capabilities: QueryCapabilities,
    pub source_count: usize,
    pub snapshot: Option<QuerySnapshot>,
}
```

```rust
pub enum QuerySnapshot {
    CanonicalEvent {
        format_version: u32,
        fact_version: u64,
        fact_rows: u64,
        layout_revision: u64,
    },
    Storyline {
        generation: String,
    },
}
```

`format_version` 透出 canonical 事件的 manifest 格式版本：当前无版本标记的既有
manifest 报告为 `1`；canonical v2 落地后写入 `2`。快照消费方据此区分事实格式，
不伪装成统一版本。

文本文件源没有独立事务快照，`snapshot` 为 `None`；Catalog 自己保留跨数据集 snapshot
标识，不伪装成单一文档源快照。

统一 SessionContext、memory/spill 配置、SQL 校验、metrics 和 catalog 调度；provider 内部优化保持分层实现。

## 9. 严格错误语义

转换层必须删除：

- `messages_value().ok()`；
- `response_value().ok()`；
- AgenticMD YAML 错误后的 `unwrap_or_default()`；
- ATIF observation 解析错误后的 `.ok()`；
- 对需要耐久保证的写入忽略 `sync_all` 结果。

新增有限的转换错误：

```rust
Error::InvalidDocument {
    format: DocumentFormat,
    path: Option<PathBuf>,
    location: Option<String>,
    message: String,
}

Error::UnsupportedCardinality {
    format: DocumentFormat,
    stories: usize,
}

Error::SourceBudgetExceeded {
    format: DocumentFormat,
    path: Option<PathBuf>,
    budget: String,
}
```

错误必须尽可能指出文件、document、record、step 或字段位置。本轮不顺带替换所有 `anyhow` 或 `Error::Other`。

## 10. 删除旧 ATIF DataSource

删除：

- `AtifDataSource`；
- `AtifDataSourceOptions`；
- `ChronicleQueryEngine::from_atif_source`；
- 专属 provider 注册、统计和仅覆盖旧入口的测试。

保留通用文件 provider、ATIF streaming parser、`AtifReader` 和加载辅助能力。内存查询测试使用临时 ATIF 文件或通用文件源。

## 11. 删除重复轨迹协议 DTO

pChronicle 删除全部 `Trajectory*Request/Response` 以及 `operations::trajectory` wire adapter。控制协议只由 `persisting-events` 定义。

CLI append 数据流为：

```text
persisting_events::TrajectoryAppendRequest
    → StoryCoords
    → pChronicle storage API
    → persisting_events::TrajectoryAppendResponse
```

禁止 serde JSON request/response transcode。Replay、stats、materialize 和 extract 如仍有调用者，保留领域函数与领域结果，但不复制 wire DTO。

## 12. 公共 API

公开架构收敛为：

```text
persisting_pchronicle::model
persisting_pchronicle::document
persisting_pchronicle::storage
persisting_pchronicle::query
```

- `model`：Storyline 与 EventRecord 相关权威类型；
- `document`：`DocumentFormat`、`DocumentSource`、打开和格式写出接口；
- `storage`：主要 store、坐标、结果和配置；
- `query`：QueryEngine、query options、backend info 和 capabilities。

Arrow row codec、Markdown AST、格式 parser、provider、lock、manifest 实现、内部常量和 projection helper 全部为 private 或 `pub(crate)`。不保留 deprecated 旧路径；当前 workspace 消费者一次迁移。

## 13. 实施顺序

### 阶段一：严格转换与 ATIF 对齐

- 先增加畸形 OpenAI、AgenticMD YAML 和 ATIF observation 的失败测试；
- 补齐 ATIF 对齐字段与缺失/null 表达；
- 删除 `_pchronicle_atif`；
- 将 ACTF/OpenAI 原始副本改成分层 residual；
- 验证三种外部格式经过 Storyline serde 和 Lance 的无损往返。

### 阶段二：AgenticMD 绑定 Storyline

- 先增加 Storyline 与 AgenticMD 完整相等测试；
- 私有化 Markdown AST；
- 删除独立 AgenticMD 领域类型；
- 迁移 Gateway 投影。

### 阶段三：统一文档源与 DataFusion

- 引入六 variant `DocumentFormat`；
- 实现 `DocumentSource` 与 query capabilities；
- 将现有 Event、Storyline 和 file providers 接入；
- 增加 AgenticMD provider；
- 收敛 QueryEngine 构造器；
- 保留每种 provider 的现有优化和真实性声明。

### 阶段四：删除重复入口和收敛门面

- 删除旧 ATIF DataSource；
- 删除重复轨迹 DTO 与 serde transcode；
- 迁移 CLI 和 workspace 消费者；
- 私有化实现模块并修复 rustdoc。

## 14. 测试与验收

### 14.1 必须覆盖的行为

1. 畸形 OpenAI JSON、AgenticMD YAML、ATIF observation 返回错误；
2. `Storyline → AgenticMD → Storyline` 完整结构相等；
3. `ATIF → Storyline Lance → ATIF` 数据模型相等；
4. `ACTF → Storyline Lance → ACTF` 保留 null、未知字段、数组顺序和 attempt 分组；
5. `OpenAI → Storyline Lance → OpenAI` 保留文件容器、多 session、ordinal 和未知字段；
6. Canonical Event 只注册 `events`，且保留 projection/filter/index/snapshot 行为；
7. Storyline Lance 保留 content late hydration 和 preview 行为；
8. ATIF 保留 streaming projection、filter pruning 和 bounded-memory 指标；
9. OpenAI/ACTF 不虚报 exact row filter pushdown；
10. AgenticMD provider 复用 Storyline 三表 schema；
11. CLI append 不再执行 serde transcode；
12. rustdoc 不再平铺或链接内部实现细节；
13. 物化与 bounded fallback 的预算耗尽返回 `SourceBudgetExceeded`，不产生静默截断；
14. residual 与正式字段冲突时产生结构化诊断，且正式字段获胜。

### 14.2 最终验证命令

```bash
cargo test -p persisting-pchronicle --no-default-features --locked
cargo test -p persisting-pchronicle --features lance-store --locked
cargo test -p persisting-pchronicle-cli --locked
cargo test -p persisting-gateway --locked
cargo clippy -p persisting-pchronicle --all-targets --features lance-store --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p persisting-pchronicle --no-deps --locked
```

Search、TTAS、Queue/Sampler 和 standalone dlcapt 的失败不属于本轮验收标准。
