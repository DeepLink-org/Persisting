# pChronicle 文档源与查询面收敛实施计划

> **供 agentic worker 使用：** 必须使用 `superpowers:executing-plans` 逐项执行；每个行为改动遵循 `superpowers:test-driven-development`，完成声明前使用 `superpowers:verification-before-completion`。

**目标：** 以 ATIF 对齐的 Storyline 为权威转换模型，消除转换层静默丢数据，统一六种磁盘文档格式及其 DataFusion 打开入口，并删除旧 ATIF DataSource、重复轨迹 DTO 和过宽公共门面。

**架构：** Canonical Event 继续作为 append-only 运行时事实源，只能投影为 Storyline；Storyline 三表 Lance 是权威轨迹模型的二进制表示；AgenticMD 是 Storyline 的 Markdown 编码；ATIF、ACTF 与 OpenAI Msg 通过 Storyline 的正式字段和分层 residual extensions 实现 JSON 数据模型级无损往返。读取使用统一 `DocumentSource`，写入保持类型化，DataFusion provider 通过能力描述共享入口但不虚报优化。

**技术栈：** Rust 2021、Serde/serde_json/serde_yaml、Arrow/Lance、DataFusion、Tokio、Cargo。

## 全局约束

- 不实施 canonical v2，不改变 Canonical Event schema、manifest、append、fence 或 segment publication 语义。
- 不进入 Search、TTAS、Queue/Sampler 或 standalone `persisting-dlcapt`。
- 不修改、删除或提交用户已有未跟踪文件。
- 不保留 deprecated 旧 API；当前 workspace 消费者同批迁移。
- 无损指 JSON 数据模型级相等，不要求空白、缩进或对象键顺序逐字节一致。
- 每个任务都先新增一个会因当前缺陷失败的测试并实际观察 RED；未观察到预期失败时停止并修正测试。
- 每个提交只包含本任务文件；提交前运行对应测试与 `git diff --check`。

---

### 任务 1：引入唯一磁盘格式枚举与严格文档错误

**文件：**
- 修改：`crates/persisting-pchronicle/src/format.rs`
- 修改：`crates/persisting-pchronicle/src/error.rs`
- 修改：`crates/persisting-pchronicle/src/tests.rs`

**接口：**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocumentFormat {
    CanonicalEvent,
    Storyline,
    AgenticMd,
    Atif,
    OpenaiMsg,
    Actf,
}

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

- [ ] 增加枚举解析/显示测试，要求只接受规范名称 `canonical-event`、`storyline`、`agenticmd`、`atif`、`openai-msg`、`actf`，并验证错误包含 format/location。
- [ ] 运行 `cargo test -p persisting-pchronicle --no-default-features document_format --locked`，确认因类型不存在而 RED。
- [ ] 实现 `DocumentFormat`、`Display`、`FromStr` 与三个错误 variant；本任务暂不删除 `ChronicleFormat`，以便后续按消费者分批迁移。
- [ ] 重跑目标测试并运行 `cargo check -p persisting-pchronicle --no-default-features --locked`。
- [ ] 提交：`refactor: define pchronicle document formats`。

### 任务 2：封堵转换层静默失败

**文件：**
- 修改：`crates/persisting-pchronicle/src/convert/openai_msg.rs`
- 修改：`crates/persisting-pchronicle/src/agenticmd/codec.rs`
- 修改：`crates/persisting-pchronicle/src/convert/atif.rs`
- 修改：`crates/persisting-pchronicle/src/store/events/manifest.rs`
- 修改：`crates/persisting-pchronicle/src/store/attempt_registry.rs`
- 修改：`crates/persisting-pchronicle/src/tests.rs`
- 修改：`crates/persisting-pchronicle/src/store/events/tests.rs`

- [ ] 增加三类 RED 测试：OpenAI record 的 `messages`/`response` 类型错误必须定位 record；AgenticMD frontmatter YAML 类型错误必须返回 `InvalidDocument`；ATIF observation 结构错误必须定位 step/observation。
- [ ] 增加目录 durability 错误的可注入单元测试：将目录同步抽为接收 `&File` 的私有 helper，helper 必须传播 `sync_all` 错误；不得依赖平台权限碰运气。
- [ ] 分别运行：

```bash
cargo test -p persisting-pchronicle --no-default-features malformed_openai --locked
cargo test -p persisting-pchronicle --no-default-features malformed_agenticmd_yaml --locked
cargo test -p persisting-pchronicle --no-default-features malformed_atif_observation --locked
```

  确认当前 `.ok()`、`unwrap_or_default()` 或忽略错误使断言失败。
- [ ] 用 `?` 和带 path/record/step location 的 `InvalidDocument` 替换静默降级；只修正持久化路径中的 `let _ = directory.sync_all()`，不机械替换语义正确的 `Option::unwrap_or_default()`。
- [ ] 重跑上述测试，再运行 `cargo test -p persisting-pchronicle --no-default-features --locked`。
- [ ] 提交：`fix: reject malformed pchronicle documents`。

### 任务 3：让 Storyline 完整表达 ATIF 字段存在性

**文件：**
- 修改：`crates/persisting-pchronicle/src/formats/storyline.rs`
- 修改：`crates/persisting-pchronicle/src/atif.rs`
- 修改：`crates/persisting-pchronicle/src/convert/atif.rs`
- 修改：`crates/persisting-pchronicle/src/store/storyline/model.rs`
- 修改：`crates/persisting-pchronicle/src/store/storyline/rows.rs`
- 修改：`crates/persisting-pchronicle/src/store/storyline/mod.rs`
- 修改：`crates/persisting-pchronicle/src/store/storyline/tests.rs`
- 修改：`crates/persisting-pchronicle/tests/atif_lance_corpus.rs`

**模型：**

```rust
#[derive(Debug, Clone, Default, PartialEq)]
pub enum FieldPresence<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

pub struct StorylineDocument {
    pub schema_version: Option<String>,
    pub attempt_id: Option<String>,
    // existing fields
}

pub struct StorylineToolCall {
    // existing fields
    pub result: FieldPresence<serde_json::Value>,
}
```

- [ ] 增加 serde RED 测试，分别输入 tool call result 缺失、显式 `null`、实际 JSON 值，要求三态序列化后仍可区分。
- [ ] 增加 split/reconstruct RED 测试，要求 `schema_version`、可选 `attempt_id` 与三种 result 状态经过 `StorylineTables` 完整相等；外部格式导入时 `attempt_id` 必须为 `None`。
- [ ] 实现 `FieldPresence<T>` 的 serde/default/skip helper；将 ATIF wire DTO 的 inline result 迁为相同存在性语义。
- [ ] 在 StoryRunRow/StoryToolCallRow 与 Arrow schema/codec 中增加列；旧数据缺列时使用 `Missing`/`None`，不得把显式 null 当缺失。
- [ ] 更新 ATIF 结构映射，删除 `_pchronicle_atif*` 原始对象副本，只映射正式字段与真正未知的 `extra`。
- [ ] 运行：

```bash
cargo test -p persisting-pchronicle --no-default-features field_presence --locked
cargo test -p persisting-pchronicle --features lance-store storyline --locked
cargo test -p persisting-pchronicle --features lance-store --test atif_lance_corpus --locked
```

- [ ] 提交：`feat: align storyline with atif presence semantics`。

### 任务 4：用分层 residual 实现 ACTF 权威往返

**文件：**
- 修改：`crates/persisting-pchronicle/src/convert/actf.rs`
- 修改：`crates/persisting-pchronicle/src/formats/actf.rs`
- 修改：`crates/persisting-pchronicle/src/tests.rs`
- 修改：`crates/persisting-pchronicle/tests/import_roundtrip_fixtures.rs`

**扩展键：** `persisting.dev/actf/v1`。该值只保存已移除正式映射键后的 document/attempt/step/tool/observation residual，不保存完整原对象。

- [ ] 增加 RED 测试：导入带未知字段、显式 null、多个 attempt 和有序数组的 ACTF；修改 Storyline 正式 message/reasoning 后导出，要求修改生效且 residual 完整保留。
- [ ] 增加断言：所有层级 `extra` 中都不存在完整源 step/trajectory，也不存在 `_pchronicle_` 键。
- [ ] 运行 `cargo test -p persisting-pchronicle --no-default-features actf_residual --locked`，确认当前 raw-step 回放使 Storyline 修改被覆盖。
- [ ] 实现“先移除 mapped keys，再保存 residual；导出先生成正式字段，再合并无冲突 residual”的双向映射。冲突时正式字段获胜，并通过 `tracing::warn!` 输出包含来源格式、源键和目标键的结构化诊断。
- [ ] 运行目标测试与 `cargo test -p persisting-pchronicle --test import_roundtrip_fixtures --features lance-store --locked`。
- [ ] 提交：`fix: make actf residual roundtrips authoritative`。

### 任务 5：用分层 residual 实现 OpenAI Msg 权威往返

**文件：**
- 修改：`crates/persisting-pchronicle/src/convert/openai_msg.rs`
- 修改：`crates/persisting-pchronicle/src/formats/openai_corpus.rs`
- 修改：`crates/persisting-pchronicle/src/tests.rs`
- 修改：`crates/persisting-pchronicle/tests/import_roundtrip_fixtures.rs`

**扩展键：** `persisting.dev/openai-msg/v1`。document residual 保存容器、相对路径和未映射 envelope；turn residual 保存 ordinal 与删除正式键后的 record residual。

- [ ] 增加 RED 测试：多 session、多 record、未知字段、显式 null 与有序数组导入后，修改 Storyline user/assistant 内容再导出；要求正式修改生效，文件分组/ordinal/residual 不变。
- [ ] 增加断言：`extra` 不含完整 raw record，不含 `_pchronicle_` 键。
- [ ] 运行 `cargo test -p persisting-pchronicle --no-default-features openai_residual --locked`，确认当前 raw record 优先导致 RED。
- [ ] 实现分层 residual 合并及 stable ordering；正式 Storyline 字段最后写入目标 map。发生键冲突时输出与 ACTF 相同字段结构的 `tracing::warn!` 诊断。
- [ ] 重跑目标测试与 import fixture 测试。
- [ ] 提交：`fix: make openai residual roundtrips authoritative`。

### 任务 6：建立三种格式经 Storyline Lance 的无损验收

**文件：**
- 新建：`crates/persisting-pchronicle/tests/storyline_lance_roundtrip.rs`
- 修改：`crates/persisting-pchronicle/src/store/storyline/mutation.rs`（仅测试暴露的缺陷需要时）
- 修改：`crates/persisting-pchronicle/src/store/storyline/rows.rs`（仅测试暴露的缺陷需要时）

- [ ] 用现有 ATIF/ACTF/OpenAI fixture 构造共享 helper：解析源 JSON Value → Storyline → 临时 Lance replace/load → 导出 Value → 语义比较。
- [ ] 先运行 `cargo test -p persisting-pchronicle --features lance-store --test storyline_lance_roundtrip --locked`，确认至少因未保存的新字段/residual 而 RED。
- [ ] 只修复 Lance split/reconstruct 或 codec 暴露的丢失，不在本任务改变 adapter 语义。
- [ ] 要求三条路径均保留 null、未知字段、数组顺序；ACTF 保留 attempt 分组，OpenAI 保留多 session 与 ordinal。
- [ ] 重跑测试，提交：`test: enforce lossless storyline lance roundtrips`。

### 任务 7：将 AgenticMD 语义接口绑定到 Storyline

**文件：**
- 修改：`crates/persisting-pchronicle/src/agenticmd/mod.rs`
- 修改：`crates/persisting-pchronicle/src/agenticmd/codec.rs`
- 修改：`crates/persisting-pchronicle/src/agenticmd/convert.rs`
- 修改：`crates/persisting-pchronicle/src/agenticmd/frontmatter.rs`
- 修改：`crates/persisting-pchronicle/src/agenticmd/body.rs`
- 修改：`crates/persisting-pchronicle/src/agenticmd/mapping/*.rs`
- 修改：`crates/persisting-pchronicle/src/lib.rs`
- 修改：`crates/persisting-pchronicle/src/tests.rs`

**公开接口：**

```rust
pub fn parse_agenticmd(input: &str) -> Result<StorylineDocument>;
pub fn encode_agenticmd(story: &StorylineDocument) -> Result<String>;
```

- [ ] 增加 RED 测试：包含 document、turn、tool call/result、observation、metrics、模型、latency、unknown extra 的 Storyline，执行 encode/parse 后完整结构相等。
- [ ] 将 `AgenticmdDocument`、`AgenticmdBlock`、`AgenticmdHeader` 降为私有 Markdown syntax AST；AST 只能在 `agenticmd/` 内出现。
- [ ] 由 frontmatter/body/comment 直接映射 Storyline 字段，删除 correlation keys 在 `turn.extra` 中的搬运。
- [ ] 保留必要的私有 byte-span/incremental edit helper，但公共函数参数和返回值不得暴露 AST。
- [ ] 运行：

```bash
cargo test -p persisting-pchronicle --no-default-features agenticmd_storyline --locked
cargo check -p persisting-pchronicle --no-default-features --locked
```

- [ ] 提交：`refactor: bind agenticmd encoding to storyline`。

### 任务 8：迁移 Gateway 到 EventRecord → Storyline → AgenticMD

**文件：**
- 修改：`crates/persisting-pchronicle/src/agenticmd/projection.rs`
- 修改：`crates/persisting-pchronicle/src/agenticmd/fs.rs`
- 修改：`crates/persisting-gateway/src/projection/markdown.rs`
- 修改：`crates/persisting-gateway/src/projection/pipeline.rs`
- 修改：`crates/persisting-gateway/src/projection/reconcile.rs`
- 修改：`crates/persisting-gateway/src/projection/frontmatter.rs`
- 修改：`crates/persisting-gateway/src/projection/dialogue/block.rs`
- 修改：`crates/persisting-gateway/src/projection/dialogue/draft.rs`
- 修改：`crates/persisting-gateway/src/projection/dialogue/tests.rs`
- 修改：`crates/persisting-gateway/tests/agenticmd_bridge.rs`
- 修改：`crates/persisting-gateway/tests/agenticmd_golden.rs`

**边界：** Gateway 只创建/修改 `StorylineDocument` 与 `StorylineTurn`；pChronicle 内部决定 Markdown block、frontmatter 和增量文件编辑。

- [ ] 先将 golden/bridge 断言改为通过 `parse_agenticmd` 检查 Storyline 语义，并运行测试确认公共 AST 仍被依赖而 RED。
- [ ] 为增量写入提供 Storyline 高层接口，例如 `upsert_agenticmd_turn(path, document_meta, turn)`；公开参数不得包含 AgenticMD AST。
- [ ] 将稳定事件投影为 Storyline turn；draft 同样使用临时 Storyline turn，完成时由同一 call id 覆盖。保持 skip/dedup/atomic rewrite 行为。
- [ ] 删除 Gateway 对 `Agenticmd*` 类型、frontmatter formatter 和 block builder 的导入。
- [ ] 运行：

```bash
cargo test -p persisting-gateway --lib projection:: --locked
cargo test -p persisting-gateway --test agenticmd_bridge --locked
cargo test -p persisting-gateway --test agenticmd_golden --locked
```

- [ ] 提交：`refactor: project gateway markdown through storyline`。

### 任务 9：实现统一 DocumentSource 和真实能力模型

**文件：**
- 新建：`crates/persisting-pchronicle/src/document.rs`
- 新建：`crates/persisting-pchronicle/src/store/document_source.rs`
- 新建：`crates/persisting-pchronicle/src/store/agenticmd_datafusion.rs`
- 修改：`crates/persisting-pchronicle/src/store/files/mod.rs`
- 修改：`crates/persisting-pchronicle/src/store/events/datafusion.rs`
- 修改：`crates/persisting-pchronicle/src/store/storyline/datafusion.rs`
- 修改：`crates/persisting-pchronicle/src/store/mod.rs`
- 修改：`crates/persisting-pchronicle/src/lib.rs`
- 新建：`crates/persisting-pchronicle/tests/document_source.rs`

**接口：** 按批准规格实现 `open_document`、`DocumentSource::{format,project_storylines,for_each_storyline,register_datafusion}`、`QueryTables`、`FilterPushdown`、`QueryCapabilities` 与私有 `QueryDocumentSource`。

- [ ] 增加六种格式打开测试及 provider 能力矩阵测试；Canonical Event 只注册 `events`，其他五种注册 `runs/steps/tool_calls`。
- [ ] 增加 AgenticMD provider RED 测试，要求它使用 Storyline 三表 Arrow schema，filter pushdown 为 Unsupported，projection 可用。
- [ ] 增加预算 RED 测试：`project_storylines` 超过行/字节预算必须返回 `SourceBudgetExceeded`；`for_each_storyline` 在同一输入上保持有界并完整遍历，不得静默截断。
- [ ] 实现私有 provider enum，复用已有 Event/Storyline/file provider；不得把一个包含互斥字段的 options struct 暴露为统一 API。
- [ ] 确保能力真值：Event 保留 exact/index/snapshot；Storyline 保留 late hydration；ATIF 为 inexact/streaming；OpenAI/ACTF 行 filter unsupported；AgenticMD 行 filter unsupported。
- [ ] 运行：

```bash
cargo test -p persisting-pchronicle --features lance-store --test document_source --locked
cargo test -p persisting-pchronicle --features lance-store --test direct_file_query --locked
```

- [ ] 提交：`feat: unify pchronicle document sources`。

### 任务 10：收敛 QueryEngine 并保持 DataFusion 优化

**文件：**
- 修改：`crates/persisting-pchronicle/src/store/query_engine.rs`
- 修改：`crates/persisting-pchronicle/src/store/local_query_manifest.rs`
- 修改：`crates/persisting-pchronicle/src/store/catalog/provider.rs`
- 修改：`crates/persisting-pchronicle/src/store/catalog/source.rs`
- 修改：`crates/persisting-pchronicle/tests/query_engine.rs`
- 修改：`crates/persisting-pchronicle/tests/direct_file_query.rs`

**公开接口：**

```rust
pub async fn ChronicleQueryEngine::open(
    format: DocumentFormat,
    path: impl AsRef<Path>,
    options: ChronicleQueryExecutionOptions,
) -> Result<Self>;
```

- [ ] 将 tests 迁到唯一 `open`，先增加 `QueryBackendInfo`/`QuerySnapshot`/capabilities 精确断言并确认现有格式专用 backend enum 无法满足；Canonical Event snapshot 必须报告 `format_version`，既有无版本 manifest 为 `1`。
- [ ] 用 `DocumentSource` 统一 SessionContext、memory/spill、SQL validation 与 metrics；provider 注册仍调用原有优化实现。
- [ ] 删除公开 `open_*`、`from_*` 构造器和 `ChronicleQueryBackend`，改用批准规格中的 `QueryBackendInfo`。
- [ ] 回归 exact filter/scalar index/pinned manifest、Storyline preview/late hydration、ATIF streaming projection/bounded metrics。
- [ ] 运行：

```bash
cargo test -p persisting-pchronicle --features lance-store --test query_engine --locked
cargo test -p persisting-pchronicle --features lance-store --test direct_file_query --locked
cargo test -p persisting-pchronicle --features lance-store --test production_scale --locked
```

- [ ] 提交：`refactor: converge pchronicle query engine entrypoints`。

### 任务 11：删除旧 ATIF DataSource

**文件：**
- 删除：`crates/persisting-pchronicle/src/store/atif_datafusion.rs`
- 修改：`crates/persisting-pchronicle/src/store/mod.rs`
- 修改：`crates/persisting-pchronicle/src/lib.rs`
- 修改：`crates/persisting-pchronicle/tests/query_engine.rs`
- 修改：`crates/persisting-pchronicle/benches/*.rs`（仅引用旧源的 benchmark）

- [ ] 先把旧源测试改为临时 ATIF 文件 + `DocumentSource`/通用 `FileTrajectoryDataSource`，保持 invalid input、duplicate step、batch size、file count 和 filter 行为覆盖。
- [ ] 删除 `AtifDataSource`、`AtifDataSourceOptions`、`from_atif_source` 及专属 re-export；保留 `AtifReader`、stream parser 和通用文件源。
- [ ] 运行 `rg -n "AtifDataSource|from_atif_source" crates/persisting-pchronicle --glob '!target/**'`，预期无输出。
- [ ] 运行 pChronicle query/direct-file tests 和 `cargo check -p persisting-pchronicle --all-targets --features lance-store --locked`。
- [ ] 提交：`refactor: remove legacy atif data source`。

### 任务 12：删除重复轨迹协议 DTO 与 serde transcode

**文件：**
- 修改：`crates/persisting-pchronicle/src/messages.rs`
- 删除：`crates/persisting-pchronicle/src/operations/trajectory/mod.rs`
- 删除：`crates/persisting-pchronicle/src/operations/trajectory/tests.rs`
- 修改：`crates/persisting-pchronicle/src/operations/mod.rs`
- 修改：`crates/persisting-pchronicle/src/operations/dispatch.rs`
- 修改：`crates/persisting-pchronicle/src/operations/bridge.rs`
- 修改：`crates/persisting-pchronicle-cli/src/control.rs`
- 修改：`crates/persisting-pchronicle-cli/src/tests.rs`

- [ ] 增加 CLI append 测试，直接构造 `persisting_events::TrajectoryAppendRequest` 并断言返回 `persisting_events::TrajectoryAppendResponse`；以类型检查保证无 serde 中转。
- [ ] 将 append/replay/stats/materialize/extract 的存储调用提取为领域函数，参数使用 `StoryCoords`、EventRecord 和领域 options/result，不复制 wire DTO。
- [ ] CLI control 对 persisting-events request 做显式字段映射，直接构建同 crate response；删除 `transcode` helper。
- [ ] 删除 pChronicle `Trajectory*Request/Response` 与 wire adapter，只保留确有调用者的领域结果类型。
- [ ] 运行：

```bash
cargo test -p persisting-pchronicle-cli --locked
cargo test -p persisting-pchronicle operations --features lance-store --locked
rg -n "struct Trajectory.*(Request|Response)|fn transcode" crates/persisting-pchronicle crates/persisting-pchronicle-cli
```

  最后一条不得命中 pChronicle 自定义 wire DTO 或 transcode。
- [ ] 提交：`refactor: use persisting events trajectory protocol directly`。

### 任务 13：完全替换 ChronicleFormat 并收紧公共门面

**文件：**
- 修改：`crates/persisting-pchronicle/src/format.rs`
- 修改：`crates/persisting-pchronicle/src/convert/mod.rs`
- 修改：`crates/persisting-pchronicle/src/formats/detect.rs`
- 修改：`crates/persisting-pchronicle/src/lib.rs`
- 新建：`crates/persisting-pchronicle/src/model.rs`
- 新建：`crates/persisting-pchronicle/src/storage.rs`
- 新建：`crates/persisting-pchronicle/src/query.rs`
- 修改：`crates/persisting-pchronicle-cli/src/exchange.rs`
- 修改：`crates/persisting-pchronicle-cli/src/lib.rs`
- 修改：`crates/persisting-pchronicle/benches/lance_vs_json.rs`
- 修改：`crates/persisting-pchronicle/benches/pchronicle_criterion.rs`
- 修改：`crates/persisting-pchronicle/benches/projection_pipeline.rs`
- 修改：`crates/persisting-pchronicle/src/formats/mod.rs`
- 修改：`crates/persisting-pchronicle/src/store/catalog/discovery.rs`
- 修改：`crates/persisting-pchronicle/src/store/catalog/identity.rs`
- 修改：`crates/persisting-pchronicle/src/store/catalog/mod.rs`
- 修改：`crates/persisting-pchronicle/src/store/catalog/source.rs`
- 修改：`crates/persisting-pchronicle/tests/atif_lance_corpus.rs`
- 修改：`crates/persisting-pchronicle/tests/s3_storage.rs`
- 修改：`crates/persisting-gateway/tests/markdown_trajectory.rs`

**公开模块：** `model`、`document`、`storage`、`query`。解析器、wire DTO、Arrow codec、provider、manifest、lock 与 projection helper 改为 private 或 `pub(crate)`。

- [ ] 增加 compile-oriented API tests，只从四个公开模块导入批准的类型和函数；删除测试对旧根级/深层路径的使用。
- [ ] 将字符串转换 API 限制到 AgenticMD/ATIF/OpenAI/ACTF；`CanonicalEvent` 与 Storyline Lance 通过类型化存储/document source，不再产生“不支持字符串 wire”的枚举分支。
- [ ] 全量迁移 CLI、Gateway 和 pChronicle tests/benches；删除 `ChronicleFormat` 定义及 re-export，不添加 deprecated alias。
- [ ] 运行：

```bash
rg -n "ChronicleFormat|Agenticmd(Document|Block|Header)|pub mod (formats|convert|store|operations)" crates/persisting-pchronicle crates/persisting-pchronicle-cli crates/persisting-gateway
cargo check -p persisting-pchronicle-cli --all-targets --locked
cargo check -p persisting-gateway --all-targets --locked
```

  `rg` 只允许命中迁移说明文字，不得命中代码标识符。
- [ ] 提交：`refactor: narrow pchronicle public facade`。

### 任务 14：文档、严格 lint 与最终验收

**文件：**
- 修改：`crates/persisting-pchronicle/README.md`
- 修改：`crates/persisting-pchronicle/src/lib.rs`
- 修改：与新 public API 直接相关的 rustdoc
- 修改：`docs/superpowers/specs/2026-08-17-pchronicle-document-source-convergence-design.md`（仅状态和最终接口有名称差异时）

- [ ] 更新 README/rustdoc：画清 Canonical Event → Storyline 单向投影、六种磁盘格式、无损边界和 provider 能力矩阵；不提 canonical v2 为现行设计。
- [ ] 运行 `cargo fmt --all -- --check` 与 `git diff --check`。
- [ ] 运行最终验收：

```bash
cargo test -p persisting-pchronicle --no-default-features --locked
cargo test -p persisting-pchronicle --features lance-store --locked
cargo test -p persisting-pchronicle-cli --locked
cargo test -p persisting-gateway --locked
cargo clippy -p persisting-pchronicle --all-targets --features lance-store --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p persisting-pchronicle --no-deps --locked
```

- [ ] 运行静态验收：

```bash
rg -n "_pchronicle_|ChronicleFormat|AtifDataSource|Agenticmd(Document|Block|Header)|fn transcode" \
  crates/persisting-pchronicle crates/persisting-pchronicle-cli crates/persisting-gateway
rg -n "messages_value\(\)\.ok|response_value\(\)\.ok|observation.*\.ok|sync_all\(\).*let _" \
  crates/persisting-pchronicle/src
```

  预期：无代码命中；若 fixture/data 中合法出现字面值，逐条人工解释，不修改数据来迎合检查。
- [ ] 确认 `git status --short` 仅包含本计划范围内变更与用户原有未跟踪文件。
- [ ] 提交：`docs: document pchronicle document source architecture`。

## 完成标准

全部 14 个任务及其 RED/GREEN 证据完成；六种 `DocumentFormat` 可通过统一读取入口打开；ATIF、ACTF、OpenAI Msg 经 Storyline Lance 的 JSON 数据模型级往返无损；AgenticMD 公共语义只暴露 Storyline；Canonical Event 和 Storyline 的 DataFusion 优化与能力声明一致；旧 ATIF DataSource、重复轨迹 DTO、serde transcode、`ChronicleFormat` 与公共 AgenticMD AST 均已删除；最终验收命令全部通过。
