# RFC-0003: pChronicle 轨迹存储层所有权

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Date** | 2026-07-31 |
| **Component** | `persisting-pchronicle` |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [RFC-0002 Events](0002-events-format.md) · [轨迹存储](../design/trajectory.md) |

## 摘要

`persisting-pchronicle` 是 Persisting 唯一的 **Agent 轨迹结构化存储层**。轨迹的逻辑记录、格式 schema、目录布局、物理落盘、读取、回放、格式转换和派生视图 MUST 收敛到 pChronicle。

Gateway、Engine 和 CLI 可以分别生产事件、编排服务和适配输入，但 MUST NOT 定义第二套通用轨迹存储实现。

## 决策

### 1. Canonical 记录

`EventRecord` 是 crate 边界上的 canonical 逻辑记录。Gateway 不再定义同构记录
schema；Gateway 内部名称 `CaptureRecord` 必须直接指向 `EventRecord`。provider/SSE payload
解释可作为 Gateway 扩展行为存在，但不得形成第二套可序列化轨迹类型。

`EventRow` 是 `events.lance` 的稳定物理行。其 Arrow schema、行转换、序号和 session 分区语义由 pChronicle 独占维护。

### 2. 物理层

| 层 | API / 实现 | 语义 |
|---|---|---|
| canonical event log | `StructuredStore`, `LanceEventStore` | append、replay、stats；同一 run 按 `session_id` 分区 |
| 人读投影 | `AgenticMdStore` | AgenticMD block 写入、读取和统计 |
| 层间操作 | `materialize_lance_to_markdown`, `compact_markdown_to_lance`, `layer_stats` | 投影、反向导入和一致性观测 |
| 发现与选择 | `expand_story_locations`, `StorageSelection` | run 分区发现、Auto 层选择和主视图策略 |
| 数据维护 | `truncate_lance_session` | session 分区级维护，不经 Gateway 转码 |
| judgment 持久化 | `JudgeRow`, `read_judge_rows`, `write_judge_rows` | judgment schema evolution、列读写及 judge unit 投影 |
| 查询视图 | `NormalizedStore` | ATIF `sessions` / `steps` / `tool_calls` 派生表 |

`events.lance` 是事实源。AgenticMD、Storyline 和 ATIF 三表均可重建，不可被当作协议级审计或回放的事实源。

### 3. 格式层

pChronicle MUST 统一拥有：

- `events`、`storyline`、`agenticmd`、`openai_msg`、`atif` 的 Rust 数据结构；
- 格式识别、校验及转换；
- event 与物理行、Markdown block、Storyline turn 之间的映射；
- AgenticMD frontmatter schema、完整文档写入、preamble 替换、分页 replay、文件枚举和结构索引；
- Python ATIF 模型与 Rust 校验规则的一致性。

外围格式间的转换 MUST 经 Storyline hub。需要保真回放的路径 MUST 直接读取 events，不得依赖有损的 Storyline roundtrip。

### 4. 组件职责

| 组件 | MUST | MUST NOT |
|---|---|---|
| Gateway | 作为 OverlayNet sink 解释并转发 Agent/LLM 协议；维护采集顺序与调用生命周期；产出 `EventRecord`；实现与实时流状态有关的 live projection 策略 | 自有网络数据面、轨迹记录或 frontmatter schema；实现通用 AgenticMD 文档 I/O、replay/stats/compact |
| Engine | 暴露稳定 ABI；把 proto 映射为 pChronicle 领域类型；实现 Lance search | 自有轨迹 service、评测算法、provider 调用、层选择、分区发现或物理存储 |
| CLI | 解析参数和输入来源；调用 Engine/pChronicle；展示结果 | 自有 Markdown/ATIF parser 或落盘协议 |
| pChronicle | 定义格式、存储、读取、转换和派生视图 | 依赖 Gateway 或 Engine 才能解释持久化数据 |

Gateway 的 live Markdown 行为可以保留 producer-specific 策略，例如流式 draft upsert；通用的 batch materialize 与 compact 仍归 pChronicle。

### 5. 代码布局

- `persisting-engine/src/search/` 是 Engine 的领域实现；`src/rpc/trajectory/` 仅包含稳定协议到 pChronicle 的适配。
- `persisting-gateway/src/session/` 维护 session 身份、路由、client metadata、索引与 snapshot。
- `persisting-gateway/src/projection/` 维护 Gateway 特有的可见文本解释、实时过滤、draft/upsert 和 reconcile。
- `persisting-gateway/src/engine/` 维护采集 actor、WAL、顺序状态机和 egress；这里的 “engine” 是 Gateway 内部编排器，不是轨迹存储层。
- `persisting-overlaynet` 是 pVisor 当前的轻量显式代理网络层，负责 CONNECT、absolute-URI forward、header 规则和网络访问策略执行；Gateway 作为 `OverlaySink` 在其上解释并转发 LLM 流量、产出轨迹事件。OverlayNet 不依赖 Gateway，可配置其他 sink。

## 一致性与故障语义

1. canonical append 成功后，派生投影失败不得回滚或伪装成 canonical 写入失败；应报告或记录 projection failure。
2. 状态机只能在 canonical append 成功后提交。
3. WAL 重启后序号 MUST 单调延续；replay 成功后 MUST ack 原 WAL entry。
4. 同一进程内 Lance 的 count-and-append MUST 串行，避免并发分配重复 `seq`。
5. ATIF 三表替换 MUST 对读者呈现单一提交点；文件后端以原子 snapshot 为权威，JSONL 是兼容投影。

跨进程并发写 Lance 尚未由进程内锁解决。部署在允许多个 writer 的拓扑中时，MUST 在更高层提供单 writer/租约，或引入支持 compare-and-swap 的提交协议。

## 收敛结果

- Engine 中原 Lance、Markdown、Arrow row 实现迁入 pChronicle；Engine 仅保留协议枚举适配器。
- Gateway 直接使用 pChronicle 的 `EventRecord`；Gateway extension 仅承载实时 payload 解释。
- Gateway 仅保留实时 payload 解释、live Markdown eligibility/upsert orchestration 与运行时 reconcile；格式解析、文件 I/O、frontmatter 契约与索引实现委托 pChronicle。
- Engine 不再 re-export materialize/compact、discovery、path、judgment 或 store API；调用方直接依赖 pChronicle。
- Python `persisting.pchronicle` 仅保留 DTO 与薄门面；ATIF 校验、拆表、三表替换、重建、联表查询和文件落盘 MUST 通过 `persisting._core` 调用 Rust pChronicle，不得保留独立 Python 实现。
- RON event lines 仅是 Engine RPC 的传输适配，不是 Engine 所有的领域模型。

新代码 SHOULD 直接依赖 pChronicle 的类型与操作；`CaptureRecord` 只是 Gateway 内部领域命名，不构成独立 public schema。

## 验收条件

- Engine 不再包含轨迹 Lance/AgenticMD/Arrow schema 实现。
- Engine 的正常和测试依赖图均不再依赖 Gateway；跨 crate fixture 兼容性由 pChronicle 的 Gateway capture corpus 测试覆盖。
- Engine 的 trajectory production code SHOULD 只包含 proto/domain 映射；append、replay、stats、judge 等流程由 pChronicle service 返回领域 outcome。
- CLI 不再实现 AgenticMD 到 event 的独立解析。
- Gateway 不再定义与 `EventRecord` 同构的序列化 struct，也不再独立实现 AgenticMD 文档重写或索引。
- pChronicle MUST 使用 Gateway 的真实 AgenticMD、request/response、provider snapshot 与 SSE fixture 验证 wire、Arrow、Lance 和投影兼容性。
- Python pChronicle 的公开门面 MUST 与 Rust pChronicle 共用同一 store 实例与校验路径。
- pChronicle 的物理后端、格式转换、WAL/投影相关边界行为有回归测试。
- 文档和 crate metadata 均把 pChronicle 表述为结构化轨迹存储层。
