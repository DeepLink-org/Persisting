# RFC-0003: pChronicle 轨迹存储层所有权

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Date** | 2026-07-31 |
| **Component** | `persisting-pchronicle` |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [RFC-0002 Events](0002-events-format.md) · [轨迹存储](../pchronicle/design/trajectory-storage.md) |

## 摘要

`persisting-pchronicle` 是 Persisting 唯一的 **Agent 轨迹结构化存储与检索层**。轨迹的逻辑记录、格式 schema、目录布局、物理落盘、读取、回放、格式转换、Search 和派生视图 MUST 收敛到 pChronicle。

Gateway 和 CLI 可以分别生产事件和适配输入，但 MUST NOT 定义第二套通用轨迹存储实现。

## 决策

### 1. Canonical 记录

`EventRecord` 是 crate 边界上的 canonical 逻辑记录。Gateway 不再定义同构记录
schema；Gateway 内部名称 `CaptureRecord` 必须直接指向 `EventRecord`。provider/SSE payload
解释可作为 Gateway 扩展行为存在，但不得形成第二套可序列化轨迹类型。

`EventRow` 是 `events.lance` 的稳定物理行。其 Arrow schema、行转换、序号和 session 分区语义由 pChronicle 独占维护。

### 2. 物理层

| 层 | API / 实现 | 语义 |
|---|---|---|
| canonical event log | `StructuredStore`, `RawEventLanceStore` | append、replay、stats；同一 run 按 `session_id` 分区 |
| 人读/调试视图 | `materialize_lance_to_markdown`, AgenticMD 文件 helpers | 从 canonical events 单向生成，可随时删除和重建 |
| 发现 | `expand_story_locations` | 发现 canonical Run/Story 分区；Markdown 不参与存储层选择 |
| 数据维护 | `RawEventLanceStore::maintain` | 显式离线 compaction、session 索引和 vacuum；事实层不支持 truncate/overwrite |
| judgment 持久化 | `JudgeRow`, `read_judge_rows`, `write_judge_rows` | 独立 `judgments.lance` 的规范化 upsert 及 judge unit 投影 |
| Storyline 三表 | `StorylineLanceStore`, `StorylineDataSource` | 原子提交并查询 `runs` / `steps` / `tool_calls` |

`events.lance` 是事实源。AgenticMD 和 Storyline 三表均可重建，不可被当作协议级审计或回放的事实源；ATIF 是互操作文档格式，不是独立存储模型。append、replay、stats 不得回退到 AgenticMD。

canonical event 写路径 MUST 是 at-least-once append-only：不得在 append 前扫描旧行或
`event_id`，不得将 ID 唯一性、重试去重、truncate 或 overwrite 作为存储语义。一个微批
只允许执行规范化、Arrow 编码、一次私有 Lance segment append 和一次 fencing manifest
CAS。索引、compaction 与 vacuum 必须由显式维护路径执行。

### 3. 格式层

pChronicle MUST 统一拥有：

- `events`、`storyline`、`agenticmd`、`openai_msg`、`atif`、`actf` 的 Rust 数据结构；
- 格式识别、校验及转换；
- event 与物理行、Markdown block、Storyline turn 之间的映射；
- AgenticMD 的宽松解析、可视化生成、preamble 更新和调试索引；

外围格式间的转换 MUST 经 Storyline hub。需要保真回放的路径 MUST 直接读取 events，不得依赖有损的 Storyline roundtrip。

### 4. 组件职责

| 组件 | MUST | MUST NOT |
|---|---|---|
| Gateway | 作为 OverlayNet sink 解释并转发 Agent/LLM 协议；维护采集顺序与调用生命周期；产出 `EventRecord`；实现与实时流状态有关的 live projection 策略 | 自有网络数据面或轨迹记录 schema；把 AgenticMD 当作可恢复事实源 |
| CLI | 解析参数和输入来源；进程内调用 pChronicle；展示结果 | 自有 Markdown/ATIF parser、落盘协议或动态 ABI |
| pChronicle | 定义格式、存储、读取、转换、Search 和派生视图 | 依赖 Gateway 才能解释持久化数据 |

Gateway 的 live Markdown 行为可以保留 producer-specific 策略，例如流式 draft upsert；它必须同时写 canonical events。通用 batch materialize 归 pChronicle，不提供 AgenticMD → events 的隐式 compact。

### 5. 代码布局

- `persisting-pchronicle/src/search/` 实现 Lance 文档写入、索引与检索；`src/operations/trajectory/` 是强类型轨迹操作适配。
- `persisting-gateway/src/session/` 维护 session 身份、路由、client metadata、索引与 snapshot。
- `persisting-gateway/src/projection/` 维护 Gateway 特有的可见文本解释、实时过滤、draft/upsert 和 reconcile。
- `persisting-gateway/src/engine/` 维护采集 actor、WAL、顺序状态机和 egress；这里的 “engine” 是 Gateway 内部编排器，不是轨迹存储层。
- `persisting-overlaynet` 是 pVisor 当前的轻量显式代理网络层，负责 CONNECT、absolute-URI forward、header 规则和网络访问策略执行；Gateway 作为 `OverlaySink` 在其上解释并转发 LLM 流量、产出轨迹事件。OverlayNet 不依赖 Gateway，可配置其他 sink。

## 一致性与故障语义

1. canonical append 成功后，派生投影失败不得回滚或伪装成 canonical 写入失败；应报告或记录 projection failure。
2. 状态机只能在 canonical append 成功后提交。
3. WAL 重启后序号 MUST 单调延续；replay 成功后 MUST ack 原 WAL entry。
4. `seq` 由 producer 定义；存储不得读取 row count 分配全局序号。writer epoch 的可见性
   必须由 manifest CAS 串行化。
5. Storyline 三表替换 MUST 对读者呈现单一提交点；`CURRENT` 指向的不可变 generation 是权威快照。ATIF 输入 MUST 先转换为 Storyline，不得维护第二套 normalized schema。

Run lease epoch MUST 通过 `EventWriterFence` 进入 canonical event 提交协议。新 epoch MUST
先以 compare-and-swap 激活 manifest；reader MUST 只读取 manifest 固定的 segment version，
不得直接打开某个 segment 的 latest version。失效 writer 的后续 Lance version 不得进入
可见快照。相同 epoch、不同 writer_id 的激活 MUST 被拒绝。

## 收敛结果

- 原 Engine 中的 Search、Trajectory 适配、Lance、Markdown 和 Arrow row 实现全部迁入 pChronicle；Engine crate 删除。
- Gateway 直接使用 pChronicle 的 `EventRecord`；Gateway extension 仅承载实时 payload 解释。
- Gateway 仅保留实时 payload 解释、live Markdown eligibility/upsert orchestration 与运行时 reconcile；格式解析、文件 I/O、frontmatter 契约与索引实现委托 pChronicle。
- 调用方直接依赖 pChronicle。
- 旧 ATIF `sessions` / `steps` / `tool_calls`、`NormalizedStore`、内存联表视图及对应 Python 门面删除；ATIF 查询统一复用 Storyline 三表 schema。
- event lines 是 pChronicle append service 的批输入表示，不是跨 crate RPC 协议。

新代码 SHOULD 直接依赖 pChronicle 的类型与操作；`CaptureRecord` 只是 Gateway 内部领域命名，不构成独立 public schema。

## 验收条件

- Workspace 不再包含 `persisting-engine` crate、动态库、C ABI 或 Engine RPC 信封。
- Search 与 append、replay、stats、judge 等流程均由 pChronicle 提供。
- CLI 不再实现 AgenticMD 到 event 的独立解析。
- Gateway 不再定义与 `EventRecord` 同构的序列化 struct，也不再独立实现 AgenticMD 文档重写或索引。
- pChronicle MUST 使用 Gateway 的真实 AgenticMD、request/response、provider snapshot 与 SSE fixture 验证 wire、Arrow、Lance 和投影兼容性。
- ATIF 与 Lance 查询 MUST 注册相同的 `runs`、`steps`、`tool_calls` Arrow schema。
- pChronicle 的物理后端、格式转换、WAL/投影相关边界行为有回归测试。
- 文档和 crate metadata 均把 pChronicle 表述为结构化轨迹存储层。
