# RFC-0006: pChronicle Vortex 轨迹后端提案

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Date** | 2026-08-16 |
| **Component** | `persisting-pchronicle` / proposed `persisting-pchronicle-vortex` |
| **Decision scope** | 可重建、读取优化的 Vortex trajectory projection；不替换 canonical facts |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [RFC-0002 Events](0002-events-format.md) · [RFC-0003 Ownership](0003-pchronicle-ownership.md) · [RFC-0005 Revision Lineage](0005-pchronicle-revision-lineage.md) |
| **External references** | [ATIF v1.7](https://github.com/harbor-framework/harbor/blob/main/rfcs/0001-trajectory-format.md) · [Vortex concepts](https://docs.vortex.dev/concepts/) · [Vortex file format](https://docs.vortex.dev/specs/file-format) |

## 1. 摘要与提议

本 RFC 提议为 pChronicle 增加一个**隔离、可选、可重建**的 Vortex 轨迹读取后端。第一阶段
Vortex 是 canonical `events.lance` 或受信交换输入的派生 projection，服务以下负载：

- 按 trajectory/session 读取完整对话、tool call 和 observation；
- 对时间、模型、source、tool、指标和 copied-context 做窄列扫描；
- 直接供 SFT/RL 读取 typed token、logprob、reward 和上下文边界；
- 在对象存储上用较少的大文件和 range read 降低 GET、listing 与小文件成本；
- 以 ATIF/Storyline 语义组织数据，同时保持物理窄列和独立投影。

本 RFC **不**提议立刻：

- 把 Vortex 加进 pChronicle 默认 feature；
- 让 Gateway 同时双写 Lance 与 Vortex；
- 用 Vortex 替换 canonical facts、writer fencing 或当前 Lance MVCC；
- 声称 Vortex 在所有轨迹、所有查询上都比 Lance 更小或更快；
- 把 Lance 与 Vortex 强行抽象成语义完全相同的一个 `StorageBackend`。

本 RFC 也不自动修改 RFC-0003 的“外围格式经 Storyline hub”规则。下面识别出的 ATIF v1.7
保真差距必须通过 Storyline/mapping 的独立 schema 决策解决；实验代码可以测量候选物理模型，
但不能绕过已接受的 production ownership contract。

建议的初始拓扑是：

```text
Gateway / importer
       │
       ▼
canonical events or fixed exchange snapshot
       │
       ├── current Storyline Lance projection
       │
       └── Vortex projector
               │
               ▼
       immutable Vortex dataset snapshot
               │
               ├── point trajectory reader
               ├── virtual runs/steps/tool_calls tables
               └── SFT/RL sequential reader
```

只有在代表性数据上满足本 RFC 的正确性、恢复、依赖隔离和性能门槛后，才另行决定是否让
Vortex 成为正式 projection，或进一步研究 direct-capture/canonical backend。

## 2. 背景与问题

### 2.1 当前模型的优点

当前 `StorylineLanceStore` 把一个 Storyline 拆成：

| 表 | 粒度 |
|---|---|
| `runs.lance` | 一条 Storyline 一行 |
| `steps.lance` | 一个 turn 一行 |
| `tool_calls.lance` | 一个 tool call 一行 |
| `objects.lance` | 内容寻址的大对象 |

三表是合理的关系投影：列窄、SQL 直观、Lance BTree/Bitmap index 可用，单个 Storyline 可以
merge-replace，大对象可以 BLAKE3 去重并延迟 hydrate。它也已经通过 `CURRENT` 固定三张业务表
和对象表的精确 version。

### 2.2 当前模型的结构成本

三表也引入了不可忽略的物理成本：

1. `run_id`、`session_id` 在 step 和 tool-call 行中重复；
2. 完整 trajectory 需要定位和读取三张 dataset，必要时再读取 `objects.lance`；
3. `message`、`metrics`、`arguments`、`results` 和 `extra` 中仍有大量 JSON 文本；
4. token id、logprob、reward 等数组若留在 JSON 中，无法充分使用 integer packing、delta、
   sparse、共享 offsets 和压缩态计算；
5. 高频 replace 会积累 fragment、delete file、index delta 和历史 version，必须显式维护；
6. 三张表、业务索引、对象表和 generation 增加 S3 object、open 和 range 请求；
7. 关系模型并不直接表达 ATIF 的 `Trajectory → steps → tool_calls / observation` 局部性。

### 2.3 不能用一个普通“宽嵌套行”解决

把整个 ATIF JSON 或一个超宽 `Struct` 作为 trajectory 行写入 Vortex，并不能自动得到好结果：

- JSON 字段仍然失去类型信息；
- 如果嵌套字段不能独立 projection，扫描一个 `source` 也可能物化 message/tool result；
- 一个超大 trajectory 可能形成超大 cell，破坏 page、缓存和并行调度；
- 默认 DataFusion 集成对复杂 nested projection 的能力不能替代领域 reader；
- “逻辑嵌套”不等于“物理宽表”，两者必须由 Layout 明确分开。

因此本提案的核心不是更换文件扩展名，而是：

> 以 trajectory 作为局部性、目录和提交单元；以 List offsets 和独立 child lane 保持物理窄列；
> 以 Vortex Layout/Encoding 组织压缩、裁剪和 range I/O。

## 3. 设计原则

### 3.1 逻辑嵌套，物理窄列

公共逻辑模型保持 ATIF/Storyline 的嵌套含义。物理模型把每层 `List<Struct>` 的 offsets 和
child fields 分开存储，因此 step/tool/result 不重复上层身份，也不要求读者一次取回整个结构。

### 3.2 DType、Encoding、Layout 分工

- **DType** 表示 ATIF/Storyline 的逻辑值和 nullability；
- **Encoding** 表示一个 array 如何压缩，例如 dictionary、sequence、run-end、sparse、FSST、
  PCodec、Zstd 或后续的 prompt-prefix encoding；
- **Layout** 决定跨 array、跨 trajectory、跨 page 的位置、chunk、统计和 I/O 局部性。

领域语义不能塞进通用压缩器；文件位置、S3 locality 也不能靠 DType 猜测。

### 3.3 不可变发布优先于对象内 append

标准 S3 `PutObject` 不能随机改写对象的一部分；S3 Express One Zone directory bucket 只支持在
当前对象末尾 append，而且有 part 数量、部署范围和存储类别约束。因此 baseline 不依赖对象内
append，而使用滚动不可变文件、不可变 manifest 和一个很小的条件更新 `CURRENT`。

### 3.4 Projection 失败不能影响事实写入

Vortex projector 固定输入事实 snapshot 后异步构建。构建、上传、索引或 `CURRENT` CAS 失败时，
旧 Vortex snapshot 继续可读，canonical facts 不回滚、不阻塞、不伪装成已经投影成功。

### 3.5 先组合内建能力，再引入自定义格式

第一阶段优先组合 Vortex 内建 DType、array encoding 和 layout node。只有 benchmark 证明通用布局
不足时，才引入自定义 `TrajectoryClusterLayout`、`PromptPrefixArray` 或 `ExternalBlobLayout`。
自定义 on-disk ID 必须版本化并注册，不能依赖未声明的进程内实现细节。

### 3.6 依赖和构建默认隔离

Vortex 不进入 pChronicle、pPilot、pVisor、Gateway 或 CLI 的默认依赖图。默认构建、测试和发布
二进制中不得出现 Vortex crate，除非消费者显式启用或构建独立实验 crate。

## 4. 范围与非目标

### 4.1 第一阶段范围

- 本地文件和 S3-compatible object store；
- 从固定 canonical snapshot 或 ATIF/Storyline corpus 构建；
- 在 manifest 声明的 capability 内完整重建 Storyline/ATIF；不支持的 hub 语义必须拒绝或显式
  quarantine，不能静默丢失；
- `runs`、`steps`、`tool_calls` 公共虚拟关系；
- 单 trajectory 点读、批量顺序读和窄列分析扫描；
- typed metrics/token lanes；
- 大对象 hot/cold/external 分层；
- generation、CAS、校验、compaction、vacuum 和 projection lineage；
- 与现有 Lance 路径的差分正确性和性能 benchmark。

### 4.2 第一阶段非目标

- 对一个已发布 `.vortex` 文件原地更新；
- 每个 Gateway SSE chunk 都更新同一个 S3 对象；
- 每步随机 update 的低延迟 OLTP；
- 全局 exactly-once、event ID 唯一性或自动 retry merge；
- 全文、向量或语义 Search；
- 依靠 Vortex 文件自身提供 table-format、catalog 或跨文件事务；
- 绕过 pChronicle reader 后仍保证外部 blob 自动 hydrate；
- 读取未知自定义 encoding/layout 时静默退化。

## 5. 逻辑模型

### 5.1 Dataset、Run、Trajectory 与 Step

Vortex projection 的逻辑根是 `TrajectoryBundle`。一个 Dataset snapshot 包含多个 bundle file，
每个 file 包含多个独立 trajectory。

```text
DatasetSnapshot
└── DataFile*
    └── Trajectory*
        ├── Agent
        ├── Step*
        │   ├── Message / Reasoning / Metrics
        │   ├── ToolCall*
        │   └── ObservationResult*
        └── Parent / Child trajectory references
```

### 5.2 身份语义

ATIF v1.7 区分 run-scoped `session_id` 与 document-scoped `trajectory_id`；兄弟 subagent 可以共享
`session_id`。当前 Storyline 使用 required `session_id` 作为 Storyline key。因此物理目录不能把
裸 `session_id` 当成跨 source 全局唯一键。

本提案定义内部键：

```text
TrajectoryKey = (
    source_id,
    run_key,
    trajectory_key
)
```

其中：

- `source_id` 来自固定输入 Source identity；
- `run_key` 优先使用 Storyline `run_id` 或 ATIF run-scoped `session_id`；
- `trajectory_key` 优先使用独立 `trajectory_id`，否则使用规范化 Storyline `session_id`；
- 两者仍不足时，importer 以 source fingerprint、文档路径和稳定 preorder ordinal 生成内部键；
- 生成键是物理 identity，不回写或伪装成生产者原始 ID。

公共 SQL 始终保留 `_file_`/Source identity；跨 Source join 不能只使用 `session_id`。

### 5.3 Subagent 拓扑

ATIF 允许递归 `subagent_trajectories`。无限递归地把整棵树放入一个 row 会导致：

- schema projection 复杂；
- 单 row 可能极大；
- subagent 无法独立定位和并行读取；
- 相同子轨迹难以复用或单独更新。

因此 writer 在 Dataset 内把递归树**拓扑扁平化**为独立 trajectory rows，并保存：

```text
parent_trajectory_key: TrajectoryKey?
child_trajectory_keys: List<TrajectoryKey>
parent_local_ordinal: u32?
child_local_ordinals: List<u32>?
producer_trajectory_id: Utf8?
producer_session_id: Utf8?
```

key reference 是跨 file 的正确性表示；local ordinal 只在 parent/child 被共同放进一个 data file 时
作为加速提示，不能单独承担身份。writer 应尽量共置一棵中小 subagent tree，但超大拓扑允许跨
file/cluster。导出 ATIF 单文件时，reader 通过 directory 解析 key 并恢复递归
`subagent_trajectories`；正常查询和训练可以独立扫描子轨迹。

### 5.4 Gateway 流与终态 trajectory

在线 Gateway 事件在收到请求时并不知道后续 SSE、tool result、subagent 或终态指标。第一阶段
Vortex projection 不要求热路径提前构造完整 ATIF。projector 从固定 canonical events snapshot
折叠出终态或截止水位的 Storyline，再写 Vortex。

如果未来引入 direct Vortex capture，应使用独立的 `GatewayExchange` 事件 DType 和滚动 segment，
而不是让未完成事件不断重写一个 `Trajectory` row。`GatewayExchange → TrajectoryBundle` 仍是显式
projection 边界。

### 5.5 当前 Hub 与 ATIF v1.7 的保真差距

本提案不能把现有转换代码尚未表达的语义假装成存储引擎已经解决。当前实现至少有两个需要在
production 之前明确处理的边界：

1. `AtifTrajectory::effective_session_id` 优先把 `session_id` 当作规范化 Storyline key，但 ATIF v1.7
   已允许兄弟 subagent 共享 run-scoped `session_id`；物理 `TrajectoryKey` 必须继续区分文档身份；
2. 当前 `split_storyline` 要求 observation result 关联同一步的 tool call，而 ATIF 允许 system action
   或非标准 action 的 result 没有 `source_call_id`。

Vortex writer 必须保留这些 ATIF 值，不能为了复用当前三表而丢弃或伪造关联。production 路径
需要先扩展 Storyline hub/mapping，形成能携带这些语义的 backend-neutral projection envelope，
再由 Vortex writer 消费。目标表示采用：

- Storyline projection envelope 保留 producer identity、document identity 和内部 key；
- observation result 物理上属于 step observation，并以 nullable call ordinal 建立可选关联；
- tool-call 内联 `result` 规范化到同一 result lane，同时保存 `result_origin`，以便按原形导出；
- full trajectory reader 能无损恢复支持范围内的 ATIF 语义；
- 三表兼容 view 只把已经关联的 result 聚合到 `tool_calls.results`。

若要让这些语义也成为 Storyline hub 和 Lance/Vortex 共同 SQL contract，应先单独演进 Storyline/
query schema 并更新 RFC-0001/RFC-0003；backend 不能私下改变既有三表的含义。Phase 1 lab 可以用
内部 direct mapper 验证 DType/Layout，但该 mapper 不注册为正式 import/query 路径。

### 5.6 Open、Terminal 与截断 trajectory

固定事实 snapshot 中的 trajectory 不一定已经结束。Vortex root 必须保存 projection completeness，
不能把“截至水位可见”伪装成 terminal：

```text
projection_state: open | terminal | truncated | invalid
source_fact_start: u64?
source_fact_end_exclusive: u64?
terminal_event_id: Utf8?
projection_warnings: List<WarningCode>
```

- `open` 可以被后续 sync 以新 revision 替换；
- `terminal` 表示 projector 已观察到受支持的终态事实，不代表未来绝不追加审计事件；
- `truncated` 表示 source cutoff、size policy 或导入声明造成不完整；
- `invalid` 默认不进入 ready snapshot，只有显式 quarantine/debug Dataset 才可保存；
- SFT/RL reader 默认只消费 `terminal`，除非调用者显式允许 open/truncated；
- manifest 记录各状态计数和 completeness contract。

## 6. 物理 DType：嵌套但不宽化

### 6.1 顶层 DType

概念上的 Vortex root 是一个 trajectory array：

```text
Struct<Trajectory> [N]
├── identity / run / lineage scalars
├── agent: Struct
├── summary: Struct
├── parent / children: TrajectoryRef + List<TrajectoryRef>
└── steps: List<Struct<Step>>
```

`steps` 虽然逻辑上嵌套，物理上由 offsets 和扁平 child arrays 表示：

```text
step_offsets: [N + 1]
step_id: [M]
timestamp: [M]
source: [M]
message_tag: [M]
message_hot: [M]
message_cold: [M]
reasoning_content: [M]
...
```

其中 `M` 是文件内全部 step 数。tool call 和 observation 分别使用第二层 offsets；result 仍属于
step observation，而不是被强制嵌入 tool call：

```text
tool_offsets: [M + 1]
tool_call_id: [K]
function_name: [K]
arguments_hot: [K]
arguments_cold: [K]

observation_offsets: [M + 1]
result_content: [R]
result_source_call_ordinal: Sparse<u32>[R]?
result_origin: [R]             # observation | inline_tool_result
result_extra: [R]
```

`result_source_call_ordinal` 指向当前 step 的 call ordinal；null 表示 system/non-tool action。读取公共
`tool_calls` relation 时只聚合非 null 且校验匹配的 result，完整 ATIF reader 则恢复全部 observation。

因此磁盘上没有在每个 step/tool row 重复 `run_id` 和 `session_id`。只有虚拟 SQL 表输出时，
`AtifSource` 才把父 trajectory 的 identity 作为 dictionary/constant array 投影给 Arrow。

### 6.2 Trajectory 顶层字段

| 字段组 | 典型字段 | 物理策略 |
|---|---|---|
| identity | source、run、session、trajectory、parent | dictionary/FSST；只存一份 |
| agent | id/name/version/default model | file/cluster dictionary |
| topology | parent/child key、本地 ordinal、continuation | key refs + primitive + List offsets |
| summary | step/tool 数、时间范围、source mask | common-source bits + `has_other`/bloom |
| training summary | copied/new steps、token counts、reward presence | typed fixed-width |
| flexible root | notes、final metrics extra、extra | typed core + JSON escape lane |

### 6.3 Step 字段

| 字段 | 首选表示 | 说明 |
|---|---|---|
| `step_id` | delta/sequence integer | 常从 1 单调递增 |
| `timestamp` | UTC millisecond + delta | 原始无法规范化时拒绝或进入显式 escape |
| `source` | small enum/dictionary | system/user/agent 等值高度重复 |
| `model_name` | dictionary | 继承 agent default 时保存 override bitmap |
| `llm_call_count` | sparse integer | null 与 0 必须区分 |
| `is_copied_context` | validity + bitmap | null 与 false 语义不同 |
| `message` | tagged content lanes | text/multimodal/empty 不混为 JSON string |
| `reasoning_content` | hot/cold content lanes | 常为空或长文本 |
| `reasoning_effort` | tagged scalar + escape | string/float union |
| `metrics` | typed core + extra JSON | token、成本、reward、logprob 可独立读 |

### 6.4 Message 与 ContentPart

ATIF message/result content 可以是 string 或 multimodal ContentPart array。不能把两者都序列化成一个
JSON cell：

```text
message_kind: [M]                  # text | parts
message_text_hot/cold: [M]?

message_part_offsets: [M + 1]
part_kind: [Q]                     # text | image | future/escape
part_text_hot/cold: [Q]?
part_media_type: [Q]?
part_source_kind: [Q]?             # relative path | CAS asset | inline producer data
part_source_ref: [Q]?
part_extra_json: [Q]?
```

空字符串、空 parts array、null lane 和 absent producer field 必须按 schema 允许范围区分。observation
result 的 `content` 复用同一 `ContentValue/ContentPart` 子结构。tool `arguments` 第一阶段仍使用经验证
的 JSON object hot/cold lane，避免在没有稳定跨工具 schema 时制造成千上万稀疏字段。

### 6.5 Metrics 与训练字段

ATIF 已明确定义 `prompt_tokens`、`completion_tokens`、`cached_tokens`、`cost_usd`、
`prompt_token_ids`、`completion_token_ids` 和 `logprobs`。这些字段不应继续整体保存在
`metrics_json` 中：

```text
metrics_present: Bitmap[M]
prompt_tokens: Sparse<u64>[M]
completion_tokens: Sparse<u64>[M]
cached_tokens: Sparse<u64>[M]
cost_usd: Sparse<f64>[M]

prompt_token_offsets: [M + 1]
prompt_token_ids: [P]          # u32/u64 由校验后的 tokenizer domain 决定

completion_token_offsets: [M + 1]
completion_token_ids: [C]
logprob_offsets: [M + 1]
logprobs: [L]                  # 与 completion token 对齐时可共享 offsets

metrics_extra_json: Utf8?[M]
```

writer 必须验证声明的长度关系。规范只给出 SHOULD 的关系若不满足，应保留独立 offsets 并记录
warning；会导致歧义或越界的值进入原始 escape/reject policy，不能截断数组使其“对齐”。无法安全
类型化的 provider-specific 值保留在 `metrics_extra_json`，不能丢弃。

### 6.6 JSON 和未知扩展

第一阶段采用“typed core + canonical JSON escape”原则：

- 规范已稳定且查询频繁的字段提升为 typed lane；
- 未知 `extra`、未来 ATIF 字段和不规则 JSON 保留为 UTF-8 JSON；
- JSON 只保证语义 roundtrip，不保证原始空白和对象 key 顺序；
- 如果上游要求字节级保真，应引用 canonical event payload，而不是经 Storyline/ATIF roundtrip；
- 不在第一阶段依赖尚未稳定的私有 Variant encoding。

### 6.7 顺序语义

- trajectory 在 data file 中的物理行顺序不构成公共 API；跨 trajectory SQL 排序必须显式
  `ORDER BY`；
- step list 保存规范化输入顺序，并有隐式 `step_ordinal`；`step_id` 仍是生产者字段，必须为正且在
  trajectory 内唯一；
- tool call 和 observation result 保留各自数组顺序，以 local ordinal 关联；
- current Storyline compatibility reader 可以继续按 `step_id/call_index` 重建其既有 contract；
- ATIF fidelity reader 使用保存的 source ordinal，不因列式写入静默改变数组顺序；
- 如果 `step_id` 与数组顺序冲突，manifest/verify 标记 `non_monotonic_step_id`，由调用者选择严格拒绝
  或保真读取，不能无提示排序后声称无损 roundtrip。

## 7. Layout：trajectory cluster 优先

### 7.1 为什么不是默认 field-first 布局

Vortex 默认文件布局大致是顶层 Struct 按字段拆分，再按 zone/chunk/compressor/buffer 组织。这对
大表分析很合理，但完整 trajectory 点读可能跨越很多远距离字段 range。反过来，把一条 trajectory
所有字节连续写成 blob 又会失去列裁剪。

本提案使用两层折中：

```text
ChunkedLayout by trajectory cluster
└── StructLayout by logical lanes
    ├── ZonedLayout / Compressor / Buffered: summary + scalar lanes
    ├── ZonedLayout / Compressor / Buffered: message/reasoning lanes
    ├── ZonedLayout / Compressor / Buffered: tool/result lanes
    └── ZonedLayout / Compressor / Buffered: token/logprob lanes
```

先按一组完整 trajectory 形成 cluster，再在 cluster 内按窄 lane 组织。点读通常只访问一个 cluster；
分析扫描仍可只读取每个 cluster 的目标 lane。

### 7.2 建议初始尺寸

以下是 benchmark 起点，不是永久格式常量：

| 单元 | 默认目标 | 边界策略 |
|---|---:|---|
| compressed data file | 256 MiB | 128–512 MiB 自适应滚动 |
| compressed trajectory cluster | 16 MiB | 8–32 MiB；不跨 trajectory 边界切分 |
| compressed lane buffer/page | 256 KiB–1 MiB | 大 payload/token lane 可更大 |
| directory shard | 16–64 MiB | 按 key range 分片 |
| blob pack | 128–512 MiB | 独立 immutable object |

一个 trajectory 超过 cluster 目标时单独成 cluster。它的逻辑 row 不拆散，但 message、result、token
等 child lane 允许继续 page/chunk，避免必须一次读取或分配整个超大 payload。

### 7.3 Layout profile

writer 可以声明 profile，但同一 Dataset snapshot 应使用单一 profile：

| Profile | 优化目标 | 代价 |
|---|---|---|
| `balanced` | 默认；点读与扫描折中 | 两类负载都不是极限 |
| `trajectory` | 完整 trajectory/S3 冷读 | 单列全库扫描产生更多 range |
| `analytics` | 大范围列扫描、训练 | 单 trajectory 可能跨更多 segment |

第一阶段只实现 `balanced`，其他 profile 先作为 benchmark 变量，避免过早形成兼容承诺。

### 7.4 内建 Layout 与自定义 Layout 的边界

Phase 1 使用内建 `ChunkedLayout`、`StructLayout`、`ZonedLayout`、`DictionaryLayout` 和
`FlatLayout` 的组合，只定制 writer 的 layout strategy 和 chunk boundary。这样普通 Vortex
工具仍能识别 layout tree。

只有以下条件同时满足才引入 `dev.persisting.trajectory_cluster.v1` 自定义 Layout：

1. 内建组合无法在 trajectory boundary 上生成所需 split/pruning；
2. 代表性 benchmark 有显著收益；
3. reader registry、兼容测试和 fallback/rebuild 路径已经存在；
4. 自定义 Layout 不依赖未版本化 Rust 类型布局。

## 8. 字段 Encoding 策略

### 8.1 默认策略

| 数据分布 | 候选 Encoding | 典型字段 |
|---|---|---|
| 小基数重复字符串 | dictionary + FSST | source、model、function name、agent version |
| 单调整数 | sequence/delta/FastLanes | step_id、timestamp、offsets |
| 大量 null | sparse/validity | metrics、reasoning、duration |
| 连续重复值 | run-end | inherited model、source runs、copied-context runs |
| 整数数组 | bit packing/PCodec | token ids、counts、ordinals |
| 浮点数组 | ALP/PCodec | cost、reward、logprobs，按实际分布选择 |
| 普通文本 | FSST 或 Zstd | message、reasoning、JSON extra |
| 已压缩/高熵 binary | identity | image、archive、独立压缩 blob |

writer 必须记录每 lane 的 logical bytes、stored bytes、chosen encoding 和 encode time，避免“编码
看起来高级但在当前分布上变大或变慢”。

### 8.2 Prompt-prefix encoding

相邻 agent step 的 `prompt_token_ids` 往往包含长公共前缀。把每一步完整 token list 独立保存会
重复历史上下文。本提案预留：

```text
PromptTokenSequence {
    base_step_ordinal: u32?
    common_prefix_len: u32
    suffix_tokens: List<u32>
}
```

为了限制随机读取的依赖链：

- 每 8–32 个 prompt 或达到字节阈值强制 checkpoint；
- 一个 prompt 最多依赖当前 trajectory 内一个早先 checkpoint/base；
- base 不能跨 file，默认也不跨 cluster；
- 解码必须验证 prefix length 和 token count；
- `take/filter` kernel 可以只解码命中 step；
- 没有至少 20% 空间收益时回退普通 List encoding。

这是 Phase 3 的自定义 Encoding 候选，不是 Phase 1 的 on-disk 要求。

### 8.3 Copied context

`is_copied_context` 是训练语义，不等同于内容去重。第一阶段仅将它作为 bitmap 和 summary count，
允许 SFT reader 在读取 payload/token 前过滤。后续可以让 copied step 引用相同 Dataset snapshot 中
的内容 hash，但不得让训练过滤语义依赖去重是否成功。

## 9. 大对象与内容层

### 9.1 三层策略

不是所有大内容都应成为一个独立 S3 object。本提案区分：

| Tier | 表示 | 适合内容 | 读取成本 |
|---|---|---|---|
| hot inline lane | 当前 cluster 的小值 page | 短 message/arguments/metadata | 最低 |
| cold in-file lane | 同一 `.vortex` 文件的独立大值 lane | 中等、通常唯一、偶尔读取 | 一个额外 range/page |
| external CAS blob pack | 内容地址 + pack/row locator | 超大、重复、附件、独立生命周期 | directory + pack range |

hot/cold 都在一个 Vortex 文件中，但使用不同 nullable child lane；扫描 summary、source 或 metrics
不会读取 cold lane。external 才是真正 offload。

### 9.2 ContentValue

逻辑内容值保持原类型，物理上使用：

```text
ContentValue {
    storage_kind: hot | cold | external
    logical_type: utf8 | json | binary | multimodal
    hot_bytes: Binary?
    cold_bytes: Binary?
    content_id: FixedBinary<32>?
    raw_length: u64
    stored_length: u64?
    codec: identity | zstd | producer
    media_type: Utf8?
    preview: Utf8?
}
```

nullable lanes由 tag 解释。reader 不得把内部 locator 暴露成用户值；`Full` 模式返回完整内容，
`Preview` 模式返回显式 preview 类型并禁止把 preview 当作完整内容执行谓词。

这里的 `FixedBinary<32>` 是逻辑记法；实现应优先使用选定 Vortex edition 可稳定表达的
`FixedSizeList<u8, 32>`/binary DType，不应只为 hash 引入私有 DType。

### 9.3 Tier 选择

默认阈值只是初始值，最终由 corpus benchmark 调整：

- 小于 8 KiB：优先 hot；
- 8–256 KiB：优先 cold；
- 大于 256 KiB：候选 external；
- 任意大小若命中已存在 content hash 且复用收益大于 locator/GET 成本，可 external；
- image/audio/archive 或 producer-compressed payload 默认 external/identity；
- 查询频繁但很大的 typed token lane不应仅因大小被当作不透明 blob。

选择器应同时考虑大小、估计重复率、查询频率、媒体类型和生命周期，而不是只使用固定阈值。

### 9.4 CAS 与 Blob pack

每个 blob 一个 S3 object 会造成 object、GET、listing 和 GC 碎片。external content 使用
128–512 MiB immutable Vortex blob pack：

```text
Struct<BlobObject>
├── content_id: FixedBinary<32>
├── logical_type
├── media_type
├── codec
├── raw_length
├── stored_length
├── preview
└── payload: Binary
```

`content_id = BLAKE3(raw bytes)`。payload 可以独立 Zstd；如果压缩无净收益则 identity。pack 按
content id 排序或分 zone，payload lane 不做二次整页压缩时必须明确标记。全局 content directory
把 hash 映射到 `(pack_id, row/zone)`；已有 hash 直接复用，不重复写入 pack。

trajectory data 只持久化 `content_id`，不持久化 pack locator；因此 blob compaction 可以移动对象并
原子发布新 content directory，而不重写全部 trajectory files。

### 9.5 一致性与 GC

发布顺序必须是：

```text
blob pack
  → blob directory
  → trajectory data files
  → trajectory directory/inventory
  → snapshot manifest
  → CURRENT
```

失败允许留下不可达 pack/file，但不允许发布悬空引用。GC 只删除所有 retained manifest 都不可达
且超过 grace period 的对象。内容 hash 碰撞、长度不匹配、解压长度错误或 checksum 错误必须失败
关闭，不能返回未经验证的字节。

### 9.6 多模态附件

ATIF multimodal content 可以用相对路径引用 image 等附件。projection builder 必须显式选择策略：

- `preserve-reference`：保存原始相对引用和 source identity，Dataset 不声明 self-contained；
- `embed-assets`：在固定 Source snapshot 中解析受限相对路径，将内容写入 CAS blob pack，并保留原始
  路径/media type provenance；
- 路径越界、绝对 URI、无法固定版本或读取失败时，`embed-assets` 构建失败，不能静默留下悬空内容。

正式 S3 Dataset 默认应使用 `embed-assets`；本地诊断 prototype 可以显式选择
`preserve-reference`。两种策略必须写入 manifest 和 projection recipe。

## 10. 文件、目录与 Dataset 布局

### 10.1 目录结构

```text
root/
├── CURRENT
├── manifests/
│   └── <snapshot-id>.json
├── inventories/
│   └── <snapshot-id>-<shard>.vortex
├── directories/
│   ├── trajectory/<snapshot-id>-<shard>.vortex
│   └── content/<snapshot-id>-<shard>.vortex
├── data/
│   └── <partition>/<file-id>.vortex
├── blobs/
│   └── <pack-id>.vortex
└── staging/
    └── <writer-id>/...
```

`data/` partition 只用于粗粒度生命周期和发现，例如日期、tenant 或 run bucket；高基数字段不能
直接形成深目录。Catalog 只把包含 `CURRENT` 的根识别为一个 source，不把内部 data/blob 文件
再次发现为独立 source。

稳态 data object 数量应近似 `ceil(compressed_data_bytes / target_file_bytes)`，而不是 trajectory 数；
directory、inventory 和 blob pack 也按目标尺寸分片。小尾文件数量持续增长视为 compaction debt，
不能用“单文件布局”掩盖。

### 10.2 `CURRENT`

`CURRENT` 是小 JSON 指针：

```json
{
  "format": "pchronicle-vortex-current/v1",
  "snapshot_id": "...",
  "manifest": "manifests/<snapshot-id>.json",
  "manifest_digest": "blake3:..."
}
```

本地通过临时文件、fsync 和 atomic rename 发布；对象存储通过 ETag/version 的 `If-Match` 或
`If-None-Match` 条件写。CAS 冲突返回 stale commit，不自动读取新状态并猜测 merge。

writer 打开 root 时必须探测/声明 conditional-write capability。不能可靠提供 CAS 的 S3-compatible
backend 只允许 create-new-root、只读，或使用另行批准的外部 fencing/transaction service；不能因
“通常只有一个 writer”而降级成无条件 last-write-wins。

### 10.3 Snapshot manifest

manifest 至少记录：

```text
format/version
snapshot_id / parent_snapshot_id
created_at / writer_id
source lineage: source URI/id, fact_version, fact_rows, projector, recipe hash
Vortex file edition and required custom registry IDs
layout profile and thresholds
inventory/directory shard refs + digests
blob directory/pack refs
row/trajectory/step/tool/blob counts
logical/stored byte totals
```

小 snapshot 可以在 manifest 内联 file inventory；超过固定数量后必须写 inventory Vortex shards，
避免一个 JSON manifest 随文件数无限增长。

### 10.4 Trajectory directory

trajectory directory 是按 `TrajectoryKey` 排序的窄 Vortex relation：

| 字段 | 作用 |
|---|---|
| key hash + identity components | 精确定位并校验 hash 冲突 |
| data file id | 目标 immutable file |
| cluster id / row ordinal | 文件内定位 |
| revision sequence | overlay 解析 |
| deleted | tombstone |
| byte/row estimates | scheduler 与代价判断 |
| summary stats | 不打开 data file 的初筛 |

directory 使用有界的 immutable LSM runs，而不是每次 sync 重写全量目录：

```text
L0: 本次/近期 sync 的小 sorted delta runs（含 tombstone）
L1+: compaction 生成的按 key range 分片 base runs
```

manifest 按新到旧记录 runs，并保存每个 shard 的 sparse fence keys。point lookup 从最新 L0 开始，
命中 key/tombstone 即停止，再查询对应 base range；full scan 做有界 merge。L0 run 数达到阈值后先合并
成新的 immutable base shards，再发布 manifest。content directory 使用相同原则，因此新增 blob 不会
重写全局 hash 目录。

### 10.5 Directory 不放文件头

标准 Vortex 文件的 layout/footer 位于文件尾，并定位数据 segments。已发布文件不可变，因此无需
“每次新 trajectory 都重写头部 directory”。writer 在本地或 multipart staging 中完成整文件，seal
一次 footer 后上传/完成 multipart，最后才由 manifest 引用。

如果 footer 或 upload 没完成，该对象没有进入 manifest；reader 仍从旧 `CURRENT` 找到完整旧
snapshot。baseline 不在同一 `.vortex` 对象中维护 footer chain，也不依赖头部指向最新 footer。

这一选择牺牲“一个对象永久 append”，换取标准格式、S3 可移植性、简单恢复和确定的 checksum。

### 10.6 可选本地流式 spool：双槽 footer pointer

如果未来 direct capture 需要“写入过程中即可恢复”，可以定义 pChronicle 私有的本地
`.pvtxlog` spool envelope，而不是修改标准 `.vortex` 文件：

```text
superblock
├── slot A: generation, footer_offset, footer_length, checksum
└── slot B: generation, footer_offset, footer_length, checksum
append-only data/segment/footer chain
```

提交一次 checkpoint 的顺序是：

1. append 完整 data segment 和新 footer，footer 反向引用上一有效 footer；
2. fsync data/footer；
3. pwrite generation 较旧的 superblock slot；
4. fsync superblock；
5. reader 选择 checksum 正确且 generation 最大的 slot，新 slot 损坏则回退旧 slot。

这正适合本地可随机写文件，但它不是标准 Vortex file，不能要求普通 Vortex reader 直接打开。
spool 达到滚动条件后应 seal/转换为标准 immutable `.vortex` 并上传。S3 Standard 不能 pwrite header；
S3 Express 的末尾 append 也不能更新这个头部槽，因此对象存储仍使用独立小 `CURRENT`/manifest，
不以 `.pvtxlog` 作为 baseline durable format。

## 11. 写入、同步与发布

### 11.1 全量构建

```text
1. 固定输入 Source/fact snapshot
2. 读取并验证 canonical events 或交换文档
3. 规范化为 Storyline/TrajectoryBundle
4. 拓扑扁平化 subagent，生成内部 ordinals
5. typed core 提取、内容分层、cluster packing
6. 写并 seal blob packs / blob directory
7. 写并 seal data files
8. 写 trajectory directory / inventory
9. 校验所有 digest、引用、计数和 roundtrip sample
10. 写 immutable manifest
11. CAS 发布 CURRENT
```

在第 11 步之前，任何 reader 都看不到新 snapshot。

### 11.2 增量 projection sync

从 canonical append suffix 计算受影响 trajectory/session。对受影响项：

- 重新投影完整 trajectory，而不是尝试原地 patch 一个 nested step；
- 写入新的 L0 data file/cluster；
- 写一个小的 sorted L0 directory run，使新 revision 指向新 row；
- 删除使用 tombstone；
- 新 manifest 保留未受影响 file 和 base directory runs，并把 L0 run 加到最前；
- 旧 row 在 retained snapshot 中仍可读，直到 compaction/retention 允许回收。

这是 projection replace 语义，不是 canonical event dedup，也不改变 producer append 顺序。

### 11.3 流式与滚动

projector 可以持续消费事实水位，但输出必须按滚动文件发布：

- 内存/本地 spool 累计到 file/时间阈值；
- seal 完整 Vortex footer；
- 上传 immutable object；
- 批量更新 directory 和 snapshot；
- 不为每个 event/trajectory CAS `CURRENT`；
- shutdown 时可以 seal 小尾文件，后续 compaction 合并。

Gateway 若未来 direct capture，也应写本地/WAL 或独立事件 segment，再异步形成 Vortex file；不能
把一个尚未完成的 S3 Vortex 对象当作唯一 durable state。

### 11.4 并发 writer

第一阶段每个 Dataset root 只支持一个 projection committer。多个 compressor/uploader 可以并行，
但最终 manifest builder 串行并以 `CURRENT` CAS fencing。stale writer 产生的 immutable objects 是
orphan，不能自动把它们 merge 到新 snapshot。

未来多 writer 需要单独定义 partition ownership 或 transaction coordinator，不由 Vortex 文件格式
本身提供。

### 11.5 内存、背压与超大 trajectory

writer 以有界 cluster builders 接收 trajectory stream：

- 在加入下一 trajectory 前根据各 lane logical bytes 估算是否 flush；
- compression/upload channel 都有 batch、in-flight bytes 和并发上限；
- hot/cold/external 选择在保留完整 payload 副本之前完成；
- JSON/ATIF 输入使用有界 record reader，不能先把整个 corpus 构造成 `Vec<Trajectory>`；
- 临时压缩、排序 directory 和 pack 可使用显式 scratch directory，并纳入磁盘 quota；
- 一个 trajectory 自身超过内存预算时，大 payload 先 spill/offload，typed child lanes使用分段 builder；
- 如果选定 Vortex API 无法在不越过预算的情况下构造该 nested row，writer 返回可诊断的
  `TrajectoryTooLarge`，不能依赖 OOM 或无界 swap。

任何 scratch 文件都属于未发布 staging；崩溃恢复只清理它们，不把部分 array 推断为已提交数据。

## 12. Update、Compaction 与 Retention

### 12.1 为什么需要 Dataset 层

Vortex 是 array/file/layout 格式，不是完整 table format。以下能力由 pChronicle Dataset 层提供：

- snapshot manifest；
- replace/tombstone overlay；
- active row directory；
- compaction；
- retention/vacuum；
- projection lineage；
- writer CAS。

不能把“Vortex 文件可读”误认为“已经获得 Lance 的 MVCC、merge 和 index 语义”。

### 12.2 Active row resolution

directory 对一个 `TrajectoryKey` 只暴露 snapshot 中 revision sequence 最大的非 tombstone row。
point read 直接命中它。分析扫描由 `AtifSource` 把 active row selections 下推到 data file；旧 file
中仍活跃的其他 trajectory 继续可读，shadowed rows 被 selection 排除。

directory LSM 的新旧顺序只用于解析同一 key，不改变事实顺序。reader 对 runs 数、总 directory
bytes 和 merge memory 设置硬上限；超过上限的 snapshot 标记 maintenance required，而不是无界打开。

随着 replace 增加，active row 可能在旧文件中越来越稀疏，range 数和 selection bitmap 成本上升，
因此 overlay 必须有界。

### 12.3 Compaction 触发建议

以下初始阈值只作为运维默认值：

- L0 data file 超过 32 个；
- trajectory/content directory L0 runs 超过 8 个；
- file active-density 低于 70%；
- shadowed/tombstoned trajectory 超过 active 的 10%；
- 平均 file 小于目标尺寸的 25%；
- point read 平均 range/GET 或 scan amplification 超过预算；
- blob pack 的不可达字节超过 20%。

compaction 从固定 snapshot 读取 active rows，重写较大的 L1 files/directory，先发布新 manifest，
再按 retention 回收旧对象。compaction 不改变 fact watermark、Storyline 语义或 revision lineage。

### 12.4 Retention 与 Vacuum

保留策略至少包含：

- 最近 N 个 snapshot；
- 最短时间窗口；
- 被打开 reader lease/pin 引用的 snapshot；
- 审计或 revision lineage 显式 pin 的 snapshot；
- orphan grace period。

vacuum 必须从所有 retained manifest 做可达性分析。仅依据最新 `CURRENT` 删除文件会破坏固定旧
snapshot 的 reader。

## 13. 查询设计

### 13.1 公共关系不改变

Vortex Source 对外注册与当前 Storyline 相同的：

```text
runs
steps
tool_calls
```

后续可以增加只读 `trajectories` 和 `observation_results` relation；前者提供 trajectory-level
nested/summary 访问，后者表达 ATIF 中未关联 tool call 的 system/non-tool results。新增 relation
需要独立 query-model 变更，不属于三表兼容承诺。现有列名、null 语义和已关联 tool result 必须与
当前 Storyline schema 差分一致。公共 query 不暴露 offsets、ordinal、file locator 或 blob descriptor。

### 13.2 专用 `AtifSource`

不能只把 nested Vortex file 注册给通用 DataFusion reader。`AtifSource` 负责把逻辑 SQL 映射到
物理 lanes：

```text
DataFusion projection / filters / limit
       │
       ▼
AtifSource planning
       ├── source/trajectory directory pruning
       ├── trajectory summary pruning
       ├── cluster/zone pruning
       ├── exact child-lane projection
       ├── active-row selection
       └── late content hydration
       │
       ▼
virtual runs / steps / tool_calls Arrow batches
```

父 identity 在输出 step/tool relation 时以 dictionary/constant array 生成，不从磁盘重复读取 M/K 次。

### 13.3 Predicate 下推等级

| Predicate | 下推位置 | 精确性 |
|---|---|---|
| `_file_` / source | Catalog | exact |
| trajectory/run/session key | directory | exact，仍校验原 key |
| time range | trajectory summary + step zones | summary conservative，DataFusion 复核 |
| source/model | summary mask/dictionary + child lane | conservative/exact |
| function name | tool bloom/dictionary + child lane | bloom conservative |
| copied context | bitmap | exact |
| typed token/cost/reward | metrics lane | exact 或 zone conservative |
| message/result JSON/content | hydrate 后 | 默认不下推 |

OR、函数、复杂 nested path 和用户 UDF 不能证明安全时，保留给 DataFusion；不能为追求 pruning 改变
SQL 结果。

### 13.4 Point trajectory read

```text
CURRENT small object
  → manifest
  → directory fence + one shard/zone
  → one data file tail/postscript/footer
  → one cluster's selected lanes
  → optional cold/external content
```

这是 cold-open 路径。`VortexTrajectorySnapshot::open` 固定并缓存 manifest、directory fences 和 file
footer；同一 snapshot 的后续 point read 不重复读取 `CURRENT`。refresh 构造新 snapshot 后再切换
consumer，不能在一次查询中观察两个 generation。

session-only lookup 若在一个 run/source 内不唯一，API 必须要求复合 key 或返回多个候选，不能任意
选择第一条。

### 13.5 训练读取

SFT/RL reader 不必构造三张虚拟关系再 join。它直接按 trajectory/step 顺序读取：

- 先读 copied-context、source、`llm_call_count` 和 metrics presence；
- 过滤 `is_copied_context = true` 和 deterministic dispatch；
- 再读取命中 step 的 message、completion tokens、logprobs、reward；
- 保持 tokenizer/model metadata 和 offsets；
- 按 cluster 产生有界 batch 与背压。

这条路径是 Vortex 相对通用三表最重要的潜在收益之一。

### 13.6 Content hydration

- 未投影内容列：不读取 hot/cold payload 或 blob pack；
- hot/cold 内容谓词：先读取完整值，再由 DataFusion 求值；
- external refs：按 pack 合并 content ids，批量读取 zone/page；
- `Preview`：只返回显式 preview relation/API，不冒充 Full；
- `LIMIT` 只有在语义安全的 filter/ordering 之后才能减少 hydration；
- checksum/长度验证失败使查询失败并标记 source unhealthy。

## 14. 索引与统计

### 14.1 Vortex 原生统计

使用 file/cluster/zone 的 min、max、null count、sort order 等统计完成粗粒度 pruning。默认 Vortex
布局和压缩态 compute 是基础，但不等价于 Lance BTree/Bitmap scalar index。

### 14.2 pChronicle summary lanes

每个 trajectory 保存小而稳定的 summary：

```text
step_count / tool_call_count
min_timestamp / max_timestamp
source_presence_mask
model dictionary ids or small bloom
tool-name bloom
copied_step_count
prompt/completion token totals
has_reward / has_logprobs / has_external_content
estimated logical/payload bytes
```

summary 是可重建的物理冗余。writer/projector recipe 变化时必须更新 lineage/version，不能让旧 summary
静默解释新字段。

data-file 内的 summary 可以引用 file-local dictionary id；复制到跨文件 directory 的 model/tool
summary 必须使用稳定 value/hash/bloom，不能把局部 dictionary code 当作全局身份。

### 14.3 Exact ID 与高选择性查询

- `TrajectoryKey`：sorted directory + fence index；
- `tool_call_id`：第一阶段没有全局 exact secondary index；先按 trajectory 或 summary 裁剪；
- 若生产负载确实需要跨 Dataset 的 tool-call point lookup，再增加独立 sorted secondary-index
  Vortex relation；
- 不为每个可能字段预建索引；每个索引都增加写入、manifest、compaction 和空间成本。

这意味着在“已知 tool_call_id、未知 trajectory”的高选择性点查上，当前 Lance BTree 可能更优。

## 15. 故障恢复与完整性

| 故障 | 可见结果 | 恢复/处理 |
|---|---|---|
| data/blob upload 中断 | 新对象不可见 | 清理 multipart/staging；旧 CURRENT 可读 |
| Vortex footer 未写完 | 文件不进入 manifest | 删除 `.partial`/orphan；不做猜测性发布 |
| directory 写入失败 | 新 data 为 orphan | 重建 directory 或 vacuum |
| manifest 写入失败 | 新 generation 不可见 | 旧 CURRENT 可读 |
| CURRENT CAS 冲突 | stale commit 失败 | 从最新 snapshot 重新规划；不自动 merge |
| CURRENT 丢失/损坏 | source unavailable | 通过显式 repair 选择并校验 manifest，不自动选“最新文件名” |
| data footer/checksum 损坏 | 命中查询失败 | 标记 source unhealthy；从 canonical facts 重建 |
| external blob 缺失 | Full 查询失败 | Preview 可明确降级；verify 报悬空引用 |
| 未注册 encoding/layout | open 失败 | 安装匹配 reader 或从 facts 重建；禁止静默误读 |
| compaction 中断 | 未发布新对象为 orphan | 旧 snapshot 不受影响 |

`verify` 至少检查：

- `CURRENT → manifest → inventory/directory → data/blob` 全链路可达；
- 所有 digest、文件 footer 和 Vortex edition；
- directory key 唯一性、active revision 和 row bounds；
- offsets 单调并与 child array 长度一致；
- tool/result correlation、token/logprob 长度关系；
- content hash、raw length、stored length 和 codec；
- manifest counts 与实际抽样/全量模式一致；
- projection lineage 对应的 canonical fact watermark。

## 16. 安全与资源边界

### 16.1 不可信输入

reader/writer 必须限制：

- schema、layout 和 nesting depth；
- trajectory、step、tool、result、token 和 blob 的最大数量/字节；
- JSON record/extra 大小；
- 解压后大小与压缩比；
- directory shard、footer、metadata 和 split 数；
- 并发 range、in-flight bytes、hydrate bytes 和输出 batch；
- recursive subagent 深度与总节点数。

### 16.2 External reference confinement

manifest、directory 和 ContentValue 只保存相对 root 的 file/pack ID。数据文件不能提供任意 URI、
绝对路径或凭据。reader 规范化后必须验证目标仍在 Dataset root/prefix 下，防止路径穿越和 SSRF。

### 16.3 Encryption

第一阶段依赖 filesystem permissions、TLS 和 object-store SSE/KMS。不能把 Vortex 文档中的细粒度
encryption 方向当作已经交付的 pChronicle 字段级加密。若有字段级需求，应另行定义 key envelope、
rotation、cache 和 pruning 的语义。

## 17. Rust crate 与 API 边界

### 17.1 实验 crate

建议新增：

```text
crates/persisting-pchronicle-vortex/
├── src/model.rs          # TrajectoryBundle DType/mapping
├── src/writer.rs         # cluster/file writer
├── src/reader.rs         # point/stream reader
├── src/directory.rs      # key/content directories
├── src/manifest.rs       # snapshot/CAS protocol
├── src/content.rs        # tiers/blob packs
├── src/datafusion.rs     # optional AtifSource
└── benches/
```

它依赖：

```toml
persisting-pchronicle = { workspace = true, default-features = false }
```

而不是让 `persisting-pchronicle` 核心反向依赖 Vortex。实验 crate 可以是 workspace member，但不得
加入 `default-members`。

### 17.2 Feature 建议

```toml
[features]
default = []
file = ["dep:vortex"]
object-store = ["file", "vortex/object_store"]
datafusion = ["file", "dep:datafusion"]
experimental-layout = ["file"]
```

实际 feature 名随选定 Vortex release 校正。原则是 local file、object store、DataFusion 和私有
layout 不应被一个 feature 无条件全部打开。

### 17.3 API 草案

```rust
let input = FixedProjectionInput::from_events(source_snapshot);
let report = VortexProjectionBuilder::new(output_uri)
    .with_layout_profile(LayoutProfile::Balanced)
    .build(input)
    .await?;

let snapshot = VortexTrajectorySnapshot::open(output_uri).await?;
let trajectory = snapshot
    .get_trajectory(&trajectory_key, ContentMode::Full)
    .await?;

let source = VortexAtifSource::from_snapshot(snapshot)?;
source.register(&datafusion_context, "dataset")?;
```

不建议第一步定义一个包含 `append/update/index/merge/transaction` 全能力的通用 `StorageBackend`
trait。Lance 与 Vortex projection 的能力不同，强行统一会产生最小公分母或隐藏昂贵 fallback。

若后续需要共享上层调用，应按 capability 拆分：

```text
TrajectoryPointRead
TrajectoryScan
ProjectionBuild
ProjectionSync
SnapshotVerify
SnapshotMaintain
ContentResolve
```

### 17.4 与现有 Catalog/QueryEngine 的接入顺序

当前 `store::catalog`、`ChronicleQueryEngine`、`StorylineDataSource` 等模块整体位于
`lance-store` feature 下，而且公开 enum/API 含有具体 Lance 名称。Phase 1/2 不修改这些入口：实验
crate 自己创建 `SessionContext` 并注册 `VortexAtifSource`。

只有 shadow benchmark 证明值得进入 Catalog 后，才提取最小 backend-neutral 读取边界：

```text
DiscoveredSource metadata
PinnedSourceRevision
NormalizedTableProvider factory
TrajectoryPointRead capability
Source health/metrics
```

Lance 的 merge、MVCC、maintenance、scalar index 和 Vortex 的 overlay、directory、compaction 不进入
共同 trait。`ChronicleQueryBackend` 可增加 `Vortex { snapshot_id }` 作为诊断信息，但不得让业务代码
通过 enum 分支重新实现 provider 行为。

## 18. 依赖、构建时间与二进制体积

### 18.1 当前基线

在本 RFC 编写时，当前 workspace 的 `cargo tree` 唯一传递 package 数约为：

| pChronicle feature | Package 数 |
|---|---:|
| `--no-default-features` | 146 |
| `lance-store` | 520 |
| `lance-store,s3-store` | 629 |

这是依赖图指标，不等于 clean build 秒数。它说明 Lance/DataFusion/cloud 已经是重依赖，不能再把
第二个完整引擎无条件链接进所有二进制。

### 18.2 有利条件

当前 Persisting 使用 Arrow 58.3、DataFusion 54.1 和 `object_store` 0.13.2；Vortex 当前开发线也
使用 Arrow 58.3、DataFusion 54 和 `object_store` 0.13.2。选择兼容 release 时，Cargo 可以复用
最重的基础依赖，而不是编译第二套 Arrow/DataFusion。

### 18.3 风险与控制

| 风险 | 控制 |
|---|---|
| Vortex 带来数十个 core/encoding crate | 独立 crate、非默认 feature、只开需要的 codec |
| Arrow/DataFusion 版本分叉 | 固定 compatible release；CI 检查 `cargo tree -d` |
| 同一二进制静态链接 Lance+Vortex | 实验 CLI/bench 独立；生产 consumer 不默认启用 |
| Vortex Rust API 变化 | pin crates.io release；适配层只在实验 crate；不跟 develop/git |
| Vortex release 抬高 MSRV | Phase 0 对照项目 toolchain/CI；不得为 lab 静默抬高默认 MSRV |
| custom layout 扩大兼容面 | Phase 1 禁用；required registry ID 写 manifest |
| workspace-wide CI 变慢 | 独立 job/cache；日常使用 targeted package commands |
| test matrix 成倍增长 | core semantic tests共享 fixture；后端专项测试独立运行 |

验收要求：默认 `cargo build`、pPilot、pVisor、Gateway 和 pChronicle CLI 的依赖图中不出现任何
`vortex-*` package；只有显式构建实验 crate 时才增加编译成本。

### 18.4 独立 helper 与同进程链接的权衡

实验阶段优先生成独立 `pchronicle-vortex` helper/bench binary，不把 Lance 与 Vortex 同时静态链接
进默认 `pchronicle`：

| 方式 | 优点 | 代价 |
|---|---|---|
| 独立 Rust helper | 默认构建和二进制隔离；崩溃边界清楚 | 需要单独分发；进程/IPC 边界 |
| 同进程可选 feature | 类型和 DataFusion 集成直接；延迟最低 | 启用时产生 fat binary 和更大 link/test 成本 |
| 动态 library/plugin | 主程序不静态携带 codec | Rust ABI/plugin 版本复杂，custom registry rollout 更难 |
| 外部 Python/`vx` 工具 | 最快验证标准文件 | 难复用 pChronicle Rust 类型和定制 reader，不适合 production |

Phase 0/1 可以同时用 Rust helper 和官方工具交叉检查文件，但正式 reader/writer 不能依赖 shelling
out 到未固定版本的外部 CLI。

## 19. 兼容性与版本策略

### 19.1 Vortex file 与 library API

Vortex 文档声明从 file format 0.36.0 起提供向后读取兼容；这不意味着 Rust library API 或自定义
plugin API 已经 1.0 稳定。pChronicle manifest 因此同时记录：

- Vortex file format edition；
- pChronicle bundle schema version；
- layout profile version；
- required custom array/layout IDs；
- minimum pChronicle reader version；
- projector recipe hash。

### 19.2 自定义 ID

建议命名：

```text
dev.persisting.atif_bundle.v1
dev.persisting.trajectory_cluster.v1
dev.persisting.prompt_prefix.v1
dev.persisting.external_content.v1
```

reader 遇到未知 major ID 必须失败。minor 演进只有在旧 reader 能正确忽略/解释时才兼容；否则创建
新 ID。不能复用同一 ID 改变 offsets、null 或校验语义。

### 19.3 可重建性是兼容保险

在 Vortex projection 尚未稳定前，canonical facts 和 projection lineage 是升级路径。breaking
layout/encoding 通过新 generation 重建，不原地猜测旧字节。只有当 Vortex 被另行批准为 canonical
后端时，才必须提供独立的长期迁移工具和更强兼容承诺。

## 20. 可观测性与运维接口

### 20.1 写入指标

- input trajectories/steps/tools/tokens/logical bytes；
- 每 lane chosen encoding、ratio、encode CPU；
- hot/cold/external bytes 和 dedup hit rate；
- cluster/file/pack sizes 与尾文件比例；
- upload bytes、requests、multipart retries；
- manifest/CURRENT CAS latency 和 conflict；
- projection lag、source fact watermark。

### 20.2 查询指标

- footer、manifest、directory cache hit；
- files/clusters/zones considered/pruned；
- range/GET count、requested/useful bytes、coalesced bytes；
- lanes projected、compressed bytes decoded；
- external hydrate ids/packs/bytes；
- active-row selection density；
- time-to-first-batch、rows/s、P50/P95/P99；
- full trajectory reconstruction latency。

### 20.3 维护指标

- L0 count、active density、shadow/tombstone ratio；
- scan amplification 和 point-read range amplification；
- orphan/unreachable bytes；
- retained snapshot count/age；
- compaction input/output bytes、write amplification、duration；
- verify coverage 和最近成功水位。

建议的独立命令面：

```text
pchronicle-vortex build
pchronicle-vortex sync
pchronicle-vortex inspect
pchronicle-vortex verify
pchronicle-vortex compact
pchronicle-vortex vacuum
pchronicle-vortex benchmark
```

在提案阶段不把这些命令加入默认 `pchronicle` CLI。

## 21. 正确性测试

### 21.1 Golden fixture

至少覆盖：

- 空 message、null 和 absent 的区别；
- system/user/agent step；
- 多 tool calls、多个 results、无 `source_call_id` 的 system observation；
- multimodal content 和外部附件；
- typed metrics、token ids、logprobs、reward、provider extra；
- `llm_call_count = 0/1/>1/null`；
- `is_copied_context = true/false/null`；
- root/subagent、共享 session、独立 trajectory id、continuation；
- hot/cold/external content；
- 未知 `extra` 和未来字段 escape；
- 超大单 trajectory 和跨多个 page 的 token/result。

### 21.2 差分测试

测试分成两组，避免把当前 Lance/Storyline 无法表达的 ATIF 值错误归类为 Vortex 差分失败。

对三表共同 contract corpus：

1. 分别构建 Lance 与 Vortex projection；
2. 对 `runs/steps/tool_calls` 执行同一 SQL；
3. 规范化 Arrow batch 顺序后逐值比较；
4. 完整重建 Storyline/ATIF 后比较语义 JSON；
5. Full/Preview/content predicate 分别校验；
6. copied-context/SFT reader 与规范过滤结果比较。

对 ATIF fidelity corpus（共享 session 的兄弟 subagent、无 `source_call_id` result、inline tool result、
multimodal attachment），Vortex full reader 与原 ATIF 做语义 roundtrip；当前 Lance/Storyline 路径若
按既有 contract 拒绝输入，应明确记录为已知 capability difference，不能通过删字段使差分“通过”。

### 21.3 Fault injection

在 blob、data、directory、manifest、CURRENT 的每个发布边界注入失败；验证旧 snapshot 始终可读，
新 snapshot 要么完整可见、要么完全不可见。并覆盖 CAS 冲突、缺失 pack、坏 digest、坏 footer、
unknown plugin、offset overflow 和解压炸弹。

## 22. Benchmark 与验收门槛

### 22.1 数据集组合

不能只用一种合成 JSON。benchmark 至少包含：

| Corpus | 特征 |
|---|---|
| chat-heavy | 短消息多、tool 少、低 token 密度 |
| tool-heavy | arguments/result 大、唯一文本多 |
| dedup-heavy | 重复 system prompt、代码、日志片段 |
| token/RL-heavy | prompt/completion ids、logprobs、reward |
| multi-agent | 深/宽 subagent 拓扑、共享 run/session |
| pathological | 单超大 trajectory、超大 blob、极端 null/高基数字符串 |

真实数据必须先脱敏，并记录字段分布、压缩前 bytes 和重复率。

### 22.2 比较口径

- Lance 先执行等价 maintain/compact/index refresh/vacuum；
- 比较 current reachable data + index + directory + blob，不把任一方旧 retained snapshot 偏置计入；
- 另报 operational footprint，包括历史 snapshot、orphan 和 compaction debt；
- cold/warm cache 分开；
- local NVMe 与 S3 分开；
- 报 GET/range/bytes，不只报 wall time；
- 报 median/P95/P99、CPU、peak RSS、encode time、write amplification；
- 使用相同 DataFusion/Arrow 版本和等价投影/谓词。

### 22.3 查询矩阵

1. exact trajectory key point read；
2. session/run 候选查找；
3. 完整 trajectory reconstruction；
4. 只读 step source/time/model；
5. tool function group/filter；
6. 内容列不投影与投影对照；
7. content predicate；
8. Full 与 Preview；
9. SFT copied-context filter + message；
10. RL token/logprob sequential scan；
11. incremental sync；
12. compaction 与 vacuum。

### 22.4 Go/No-Go

正式 projection 必须同时满足：

- 所有差分与故障测试通过；
- 默认构建依赖图和产物不含 Vortex；
- S3 不出现 per-trajectory/per-blob 小对象退化；
- point trajectory P95 不劣于压实 Lance 20% 以上；
- narrow analytical scan 至少一个核心生产查询提升 2 倍；
- 以下三项至少再满足一项：
  - 典型 corpus current reachable bytes 不超过 Lance 的 80%；
  - S3 完整 trajectory P95 提升至少 1.5 倍或 GET 减少至少 50%；
  - token/RL 读取吞吐提升至少 3 倍；
  - projection build/ingest 吞吐提升至少 2 倍；
- compaction 后 scan amplification、active density 和恢复时间在运维预算内。

若收益只出现在合成 benchmark，或需要默认启用不稳定 custom layout 才达到门槛，则保持实验状态。

## 23. 性能与压缩预期：假设而非承诺

基于当前 schema、Vortex 编码能力和公开通用 benchmark，可以提出待验证假设：

| 负载 | Vortex/Lance 空间假设 | 主要变量 | 信心 |
|---|---:|---|---|
| 普通 chat/tool trajectory | 0.70–0.90 | ID 去重、typed core、文本占比 | 中低 |
| 大量唯一大 payload | 0.85–1.10 | 双方 Zstd、外部 CAS/pack | 低 |
| 重复 tool result/prompt | 0.60–0.90 | 全局 content dedup 命中率 | 低 |
| token/RL-heavy | 0.35–0.65 | typed arrays、prefix 重复率 | 中低 |

| 查询 | 相对假设 | 主要限制 |
|---|---|---|
| exact indexed point lookup | 持平到小幅改善/退化 | directory cache；Lance BTree 很强 |
| 完整 trajectory | 1.5–3 倍潜力 | cluster locality、content bytes |
| 本地窄列扫描 | 2–5 倍潜力 | child projection、压缩态 kernel |
| S3 窄列扫描 | 1.1–2 倍潜力 | 网络与 GET 主导；通用公开结果并不保证大幅领先 |
| SFT/RL | 2–6 倍潜力 | typed token lane、copied-context early filter |
| 随机修改单 trajectory | Lance 更有利 | Vortex 需要 overlay/compaction |

这些范围不能写进产品 SLA，也不能作为跳过本项目 benchmark 的理由。Vortex 官方 benchmark 是
方向性证据，不是 pChronicle 轨迹负载的替代测量。

## 24. 方案权衡

### 24.1 选择的方案：Vortex 可重建 projection

**优点**：

- 风险与 canonical capture 隔离；
- 可以充分定制 ATIF-aware DType/Layout；
- 可用真实数据与 Lance 做同语义差分；
- 失败时从 facts 重建；
- 不要求立即实现 Vortex 上的完整 OLTP/MVCC。

**缺点**：

- 同一事实可能同时存在 Lance 与 Vortex projection，占用额外空间；
- 需要 projector、lineage、lag 和维护；
- 生产系统短期存在两种读取实现；
- 只有通过 benchmark 后才产生实际用户价值。

### 24.2 直接替换 Storyline Lance

**优点**：少一套正式 projection，长期架构更单一。

**缺点**：在 point index、replace、MVCC、content GC、S3 commit 和 DataFusion nested pushdown 未被
证明前风险过高；回滚和数据迁移复杂。当前不选。

### 24.3 把当前三张窄表原样写成三个 Vortex 文件

**优点**：实现简单，公共 SQL 几乎不变。

**缺点**：保留三次 open/join、ID 重复和跨表 snapshot；没有利用 ATIF 局部性，收益主要只来自
codec。它适合作为 benchmark baseline，不作为目标设计。

### 24.4 一个 trajectory 一个 Vortex 文件

**优点**：点读直观、单条替换简单、文件语义清楚。

**缺点**：S3 object/listing/GET 和 footer 小文件碎片严重，训练/扫描调度成本高。当前不选。

### 24.5 一个永久增长的 Vortex 单文件

**优点**：object 数最少，顺序 append 理论简单。

**缺点**：标准 S3 不能随机更新头/footer；S3 Express append 有范围和 part 限制；故障恢复、并发、
目录增长、vacuum 和长期 compaction 都复杂；已发布 reader 与 writer 相互牵制。当前不选。

### 24.6 只保留 Lance

**优点**：依赖、运维和语义最简单；已有 MVCC、index、blob、DataFusion 和维护路径。

**缺点**：不验证 trajectory-aware layout、typed training lanes 和更少 S3 I/O 的潜力。如果真实负载
主要是随机更新/高选择性点查且没有大规模训练扫描，这仍可能是最终正确选择。

### 24.7 Parquet

**优点**：生态成熟、工具广、长期兼容清晰。

**缺点**：对自定义压缩态 kernel、trajectory-specific layout 和 plugin encoding 的扩展空间较小；
同样需要 Dataset manifest、blob、index 和 update 语义。应加入 benchmark，但不是本提案目标。

## 25. 分阶段实施

### Phase 0：依赖与可行性探针

- 独立 crate，Vortex 非默认依赖；
- 锁定与 Arrow 58.3/DataFusion 54/object_store 0.13.2 兼容的 crates.io release；
- 记录 clean build、增量 build、package graph、二进制和 target 增量；
- 写最小 nested `TrajectoryBundle`，用 `vx`/Rust reader 检查 layout tree；
- 不修改 Gateway、pPilot、pVisor 或默认 CLI。

### Phase 1：标准 Vortex 单文件 codec prototype

- Storyline/ATIF → nested offsets/lane arrays；
- balanced built-in layout strategy；
- 本地 file writer/reader；
- 完整 roundtrip、point trajectory 和顺序训练 reader；
- 无 external blob、无 DataFusion、无 custom layout；
- 与“三表 Vortex baseline”和压实 Lance 比较。

### Phase 2：虚拟关系与 typed training lanes

- `AtifSource` 注册 `runs/steps/tool_calls`；
- projection/predicate/limit 下推；
- typed metrics/token/logprob；
- summary lanes 和 point directory prototype；
- Lance/Vortex SQL 差分测试。

### Phase 3：Dataset snapshot 与 object store

- immutable manifest、directory、inventory、CAS `CURRENT`；
- S3 range/coalescing/cache；
- hot/cold/external content 和 blob packs；
- verify、fault injection、orphan cleanup；
- 全量与增量 projector lineage。

### Phase 4：Shadow production

- 从同一 canonical fact snapshot 异步构建；
- 用户流量仍读 Lance，Vortex 只 shadow query/benchmark；
- 采集压缩、延迟、GET、CPU、失败、projection lag 和维护数据；
- 不允许 projection 失败影响 capture。

### Phase 5：决策

根据 Go/No-Go：

1. 未达标：删除/保留 lab，不增加产品后端；
2. 部分负载达标：作为训练/分析专用 projection；
3. 全面达标：进入正式 Catalog Source，仍不默认替换 facts；
4. 若要成为 canonical backend：新 RFC 定义 append、fencing、repair、长期兼容和迁移。

## 26. 开放问题

1. balanced cluster 的最佳 file/cluster/page 尺寸是多少，是否应按 payload/token 分布自适应？
2. Phase 1 的 built-in LayoutStrategy 能否充分表达 child-lane projection，何时必须 custom Layout？
3. `trajectory_id/session_id/run_id` 在当前 Storyline hub 与 ATIF v1.7 之间是否需要进一步补强？
4. content tier 应由静态阈值、采样重复率，还是 workload profile 决定？
5. blob directory 应在每个 Dataset 内，还是允许多个 Dataset 共享一个 CAS domain？
6. prompt-prefix encoding 的 checkpoint 和随机读取预算如何确定？
7. 跨 trajectory 的 system prompt/tool definition dictionary 是否值得其引用和 GC 复杂度？
8. tool-call exact secondary index 是否有真实生产需求，还是 trajectory key + summary 已足够？
9. active-row directory 的 LSM/overlay 层数上限和 compaction budget 是多少？
10. Vortex custom registry 的长期读兼容、edition 和 reader rollout 如何管理？
11. 是否需要独立的 portable export profile，只使用 Vortex core edition、禁用私有 encoding？
12. S3 Express append 是否值得作为可选 spool 优化，还是始终保持 immutable object？

## 27. 决策记录

若本 RFC 被接受，它只批准：

1. 创建隔离、非默认的 Vortex 实验 crate；
2. 以可重建 projection 身份实现和 benchmark 本文物理模型；
3. 在不影响 canonical capture 的条件下进行 shadow 验证；
4. 使用 immutable files + manifest + CAS `CURRENT`，不使用永久单文件 append；
5. 保持逻辑嵌套、物理窄 lane、大文件聚合和显式 content tiers。

它不批准默认依赖、生产切流、Lance 删除或 Vortex canonical ownership。那些动作必须由测量结果和
后续决策单独批准。

## 28. 参考资料

- [pChronicle 轨迹存储设计](../pchronicle/design/trajectory-storage.md)
- [Storyline 三表 Lance 存储](../pchronicle/design/storyline-lance.md)
- [pChronicle 架构](../pchronicle/design/architecture.md)
- [Harbor ATIF v1.7 RFC](https://github.com/harbor-framework/harbor/blob/main/rfcs/0001-trajectory-format.md)
- [Vortex Arrays](https://docs.vortex.dev/concepts/arrays)
- [Vortex Layouts](https://docs.vortex.dev/concepts/layouts)
- [Vortex File Format concepts](https://docs.vortex.dev/concepts/file-format)
- [Vortex File Format specification](https://docs.vortex.dev/specs/file-format)
- [Vortex DataFusion integration](https://docs.vortex.dev/developer-guide/integrations/datafusion)
- [Extending Vortex](https://docs.vortex.dev/developer-guide/extending/)
- [Vortex benchmark dashboard](https://bench.vortex.dev/)
- [Amazon S3 PutObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_PutObject.html)
- [S3 Express append](https://docs.aws.amazon.com/AmazonS3/latest/userguide/directory-buckets-objects-append.html)
