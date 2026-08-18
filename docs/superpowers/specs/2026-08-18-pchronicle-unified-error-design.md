# pChronicle 轻量错误与边界结果设计

## 1. 设计边界

pChronicle 内部不提供统一的机器分类错误对象。稳定错误分类只属于 HTTP、CLI 和 Gateway
等外部协议边界。内部 API 分别表达可预期结果与操作失败：

- 可预期结果使用 `Option`、局部验证类型或模块局部 Outcome；
- 无法完成操作的失败使用 `anyhow::Error` 及其 source chain；
- 外部响应使用不携带 source 的边界 DTO。

TTAS、tiered tensor memory、通用 Queue/Sampler、Search 和独立的 dlcapt 组件不共享该设计。

设计遵守以下不变量：

- 正常业务代码不维护协议 code、HTTP status、传播 frame 或结构化错误上下文；
- 边界不从错误文本、source 类型或后端 variant 反向推断业务结果；
- 只有调用方需要分支处理的状态才进入 `Option` 或 Outcome；
- 未被显式表达的失败在外部边界统一视为 `internal`；
- 原始失败通过 source chain 保留，调用路径和资源字段由 tracing span 记录；
- 公开响应文本与内部诊断文本完全分离。

## 2. 内部结果模型

### 2.1 操作失败

pChronicle 的普通失败使用 `anyhow::Result<T>`。crate 可以保留便捷别名：

```rust
pub type Result<T> = anyhow::Result<T>;
```

该别名不定义新的 `Error` 类型，不附加错误码，也不改变 source。文件系统、serde、Lance、
DataFusion、Arrow、Tokio 和其他依赖错误通过 `?` 自然进入 source chain。

上下文只在以下位置添加一次：

- 模块入口；
- 资源所有权转换，例如从对象存储字节进入 manifest decoder；
- 异步 worker 或 blocking task 的 join 边界；
- 外部库 trait 与 pChronicle API 的桥接边界。

同一函数内连续调用不逐行添加 operation。上下文必须说明当前抽象正在完成的动作或拥有的资源，
不得复制下层错误文本。

```rust
let manifest = read_manifest(path)
    .with_context(|| format!("read event manifest from {}", path.display()))?;
```

### 2.2 资源缺失

查询型 API 使用 `Result<Option<T>>` 表达缺失：

```rust
pub async fn load_storyline(key: &StorylineKey) -> Result<Option<StorylineDocument>>;
```

`None` 只表示查询目标不存在。读取失败、解码失败或存储状态不一致仍返回 `Err`。

### 2.3 输入问题

parser 和 validator 可以定义局部、具体的输入问题类型：

```rust
pub fn decode_input(
    format: DocumentFormat,
    input: &[u8],
) -> std::result::Result<StorylineDocument, InputIssue>;
```

`InputIssue` 只包含生成安全公开提示所需的信息，例如字段位置和验证原因。它不包含 HTTP status、
全局 code、传播 frame 或后端错误。读取输入介质失败不属于 `InputIssue`，而是普通操作失败。

### 2.4 模块局部 Outcome

冲突、能力拒绝、容量拒绝和明确的可用性状态位于成功通道。Outcome 定义在拥有该决策的模块旁边，
只包含调用方实际处理的分支：

```rust
pub enum AppendOutcome {
    Accepted,
    Full,
    Unavailable,
}

pub async fn append(event: Event) -> Result<AppendOutcome>;
```

需要返回值的命令可以使用携带值的 Outcome。单一缺失分支使用 `Option`，不得为其创建 Outcome。
不同模块不得共享一个全局万能 Outcome。若调用方不会针对某个状态分支处理，该状态必须作为普通
成功值的一部分或操作失败表达，不能新增分类。

典型的局部结果包括：

- append admission 的 accepted/full/unavailable；
- projection 或 manifest publication 的 published/conflict；
- 查询输出的 complete/limit-exceeded；
- import 的 accepted/invalid/unsupported。

## 3. 数据流

内部数据流只进行正向决策：

```text
leaf operation
  ├─ value -----------------------------> service value
  ├─ expected local condition ----------> Option / Outcome
  └─ operational failure + source ------> anyhow::Error

service boundary
  ├─ value -----------------------------> success response
  ├─ Option / Outcome ------------------> explicit protocol response
  └─ anyhow::Error ---------------------> internal response + diagnostic log
```

服务层不得捕获任意错误后重新分类。外部边界只能依据当前函数签名中的显式结果进行协议决策。

示例：

```rust
match queue.append(event).await? {
    AppendOutcome::Accepted => Ok(CaptureResult::Accepted),
    AppendOutcome::Full => Ok(CaptureResult::ResourceExhausted),
    AppendOutcome::Unavailable => Ok(CaptureResult::Unavailable),
}
```

`?` 传播的失败不会进入上述业务匹配，并在最外层统一成为 `internal`。

## 4. 外部协议

### 4.1 稳定分类

外部协议只公开七类决策：

| Code | HTTP | 来源 |
|---|---:|---|
| `invalid_request` | 400 | 输入解析或验证结果 |
| `not_found` | 404 | lookup 的 `None` |
| `conflict` | 409 | 明确的领域 Outcome |
| `unsupported` | 422 | 明确的领域 Outcome |
| `resource_exhausted` | 429 | 容量或预算 Outcome |
| `unavailable` | 503 | 明确的可用性 Outcome |
| `internal` | 500 | 任意未预期操作失败 |

I/O、持久化损坏、未知后端故障和内部不变量失败不构成不同的公开决策，统一映射为 `internal`。

### 4.2 边界 DTO

边界 DTO 仅表示响应，不实现 `std::error::Error`，也不包装内部 source：

```rust
pub struct BoundaryProblem {
    pub code: BoundaryCode,
    pub message: String,
}
```

HTTP、CLI JSON 输出和 Gateway 可以拥有各自的传输表示；共享 wire contract 时可以共享
`BoundaryCode`。该类型不得被内部存储、格式或查询 API 使用。

### 4.3 公开消息

- `internal` 和 `unavailable` 使用固定公开文本；
- 其他响应只能使用 Outcome 或 `InputIssue` 提供的已审查公开文本；
- 任意 `anyhow::Error::to_string()`、source 文本、路径或后端诊断不得进入响应；
- 内部诊断记录与公开响应在独立代码路径生成。

## 5. 诊断与可观测性

长生命周期操作和模块入口使用 tracing span 记录运行上下文，例如 dataset URI、session、format、
path 和 worker partition。错误值不复制这些字段。

操作失败只在拥有请求或任务生命周期的最外层记录一次。日志使用 `anyhow` 的标准 Debug/chain
表示，不实现自定义 source 遍历、环检测、深度限制或诊断字符串拼接。

CLI 默认展示简洁错误；详细模式展示标准 source chain。HTTP 记录内部失败后返回固定响应。
Gateway 依据显式 Outcome 执行拒绝、重试或停止策略，意外 `Err` 作为内部失败处理。

## 6. DataFusion 与外部 trait

DataFusion trait 要求 `DataFusionError` 时使用私有薄桥接：

- pChronicle 失败进入 trait 时包装为 `DataFusionError::External`；
- trait 结果返回 pChronicle 时恢复原始失败；
- 无法取得原始所有权时，将 DataFusion error 整体保留为新的 `anyhow` source；
- 桥接只保留 source，不分类、不复制上下文、不重建 operation/message；
- SQL 输入是否合法由调用该 SQL 的边界或局部输入类型决定，不由桥接器推断。

该桥接不得识别 Lance、object-store 或 DataFusion variant 来生成外部协议 code。Arrow writer、
bounded output 等接口若借用 I/O error 表达控制信号，必须在拥有该 writer 的模块内将其转换为显式
Outcome；边界不得从 I/O error 文本恢复预算状态。

## 7. 模块契约

### 7.1 格式与转换

不可信输入的 parser 返回局部 `InputIssue`。已进入内部模型后的转换失败使用 `anyhow::Result`。
字段位置只存在于 `InputIssue` 的公开安全描述中，不在传播层重复组合。

### 7.2 存储、Catalog 与 Projection

lookup 返回 `Result<Option<T>>`。CAS、epoch 或 publication 冲突使用所属命令的局部 Outcome。
manifest、row 或 schema 解码失败作为带 source 的普通操作失败。存储 adapter 不维护统一分类表。

### 7.3 Query 与 DocumentSource

查询规划和执行失败使用 `anyhow::Result`。用户输入验证在执行前产生 `InputIssue`。行数或字节预算
由执行模块返回显式 limit Outcome。DocumentSource 不在每个 stream/collect 调用上重新包装错误。

### 7.4 Append queue

queue admission 返回局部 `AppendOutcome`。durable append 已被接受后的存储失败属于操作失败，
通过 waiter 的 source chain 传递。receiver 断开和 worker lifecycle 只在调用方需要采取可用性动作时
返回 `Unavailable` Outcome，否则作为任务失败处理。

## 8. 实现约束

- 删除统一 `Error`、`ErrorCode`、`ErrorContext`、`ResultContext`、frames 和 diagnostics；
- 删除以分类或复制诊断元数据为目的的 `map_err`；
- 不提供字符串分类器、source-chain 分类器或后端 variant 到协议 code 的中央映射器；
- 全局错误构造器数量为零；
- `store/error_adapter.rs` 删除，或缩减为约百行以内的纯 DataFusion source 桥接；
- `pchronicle::Result<T>` 若保留，只能是 `anyhow::Result<T>` 的别名；
- 正常业务函数不得引用 `BoundaryCode`、HTTP status 或边界响应类型；
- tracing 字段不得为了构造错误而再次复制到错误对象。

## 9. 验证契约

行为测试覆盖：

- lookup 缺失通过 `None` 到达 `not_found`；
- 输入问题通过局部验证类型到达 `invalid_request`；
- conflict、unsupported、resource-exhausted 和 unavailable Outcome 到外部协议的穷尽映射；
- 任意未预期失败统一到达 `internal`；
- 500/503 响应不包含路径、source 或后端文本；
- 内部日志或 CLI 详细模式保留原始 source chain；
- DataFusion External 往返保留失败链但不产生分类；
- append queue、projection、query budget 的 Outcome 不依赖错误文本；
- parser、storage、DocumentSource、HTTP、CLI 和 Gateway 的正常路径不受影响。

静态检查确认：

- 旧统一错误类型、构造器和传播 helper 不存在；
- 无基于 `Display`、`to_string()` 或 source downcast 的协议分类；
- 无仅用于复制 operation/frame/context 的 `map_err`；
- DataFusion 桥接不包含协议 code；
- pChronicle、CLI 和 Gateway 的测试及 `-D warnings` Clippy 通过。
