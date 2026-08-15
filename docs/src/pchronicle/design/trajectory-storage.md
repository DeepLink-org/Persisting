# pChronicle 轨迹存储

> 当前实现说明。规范性所有权见 [RFC-0003](../../rfcs/0003-pchronicle-ownership.md)，
> Dataset 命令见 [`pchronicle`](../reference/cli.md)。

## 1. 定位

`persisting-pchronicle` 是 Agent 轨迹的结构化存储层，统一拥有：

- `EventRecord` 逻辑事件和 Lance `EventRow` schema；
- Run / Story 坐标、目录布局和发现规则；
- Lance canonical events 的读写、统计和维护；
- AgenticMD 人读/调试视图的生成与宽松解析；
- events、Storyline、ATIF、ACTF、OpenAI messages、AgenticMD 之间的格式转换；
- materialize、judgment 和标准查询视图。

Gateway 负责把协议流量解释为事件；CLI 只解析参数与展示结果，
并在进程内直接调用 pChronicle。它们都不定义第二套轨迹落盘格式。

## 2. 逻辑坐标

```text
Run
└── Storyline
    └── Turn
        └── Call
            └── EventRecord
```

离线存储使用 `StoryCoords`：

| 字段 | 含义 |
|---|---|
| `storage` | pChronicle 根目录 |
| `agent_id` | Agent 身份，单路径段 |
| `root_session_id` | 可选 Run 身份；主 Agent 与 subagent 共用 |
| `session_id` | Story 身份，也是 Lance 分区键 |

有 `root_session_id` 时，多个 Story 共用一个 Run 级 `events.lance`；没有时，
`session_id` 自身就是目录边界。

## 3. 物理表示

### Lance events

`events.lance/` 是完整事件表示的 Run 级容器：`_manifest.json` 保存 active writer fence
和可见 segment version，每个 writer epoch 使用独立 Lance segment。它保留 HTTP/模型调用、
时间、身份、payload 和顺序。
物理 schema 把 `event_id` 提升为独立业务列，但事实层不检查唯一性，也不为它维护索引；
重复 ID 和重试行是合法事实。完整 `EventRecord` 仍保存在 `payload_json`，因此回放不丢字段。评测结果写入同 Run 的
`judgments.lance/`，不会随 rubric 增加而演化事实表 schema。需要审计保真度的工作流应
使用 canonical events 层。

### AgenticMD

AgenticMD 是面向人的 Markdown 调试视图。它保存可见对话块和会话摘要，适合实时查看、
代码审阅与人工分析。它会省略协议噪声，字段也允许缺失或扩展，因此不是存储格式或
原始 HTTP 事件的无损替代。

`pvisor run --chronicle-mode lance` 写 canonical Lance events；
`--gateway-stream-markdown` 可同时维护 live AgenticMD。Markdown 是诊断投影，Dataset
消费统一使用 pChronicle API 和 `pchronicle` 命令。

### Storyline 三表 Lance

`StorylineLanceStore` 提供面向分析和 ATIF 互操作的规范化物理表示：
`runs.lance`、`steps.lance`、`tool_calls.lance`。它按 `source_call_id` 将 observation
result 归并到 tool call 行，并通过 `CURRENT` 中的三表版本元组保证原子切换。超过阈值的
UTF-8/JSON cell 以 BLAKE3 内容地址外置到共享 `objects.lance`，跨轨迹复用；公开 schema
和 SQL 结果保持不变，查询只在真正引用内容列时延迟恢复 Blob。

ATIF object、array、pretty JSON 与 JSONL/NDJSON 的 `steps` 临时查询还支持
projection-aware 快路径：DataFusion 先传递所需列和安全谓词，reader 通过 seeded visitor
跳过未引用大字段并直接构造窄 Arrow batch；JSONL/NDJSON 逐记录有界读取，array 通过
结构扫描器逐 element 提取并使用 slice decoder，单 object 从 reader 流式解码。
`SELECT *`、其他表和格式回退到完整 Storyline 规范化。详细协议、发布顺序和执行边界见
[Storyline 三表 Lance 存储](storyline-lance.md)。

## 4. 目录布局

扁平 Story：

```text
storage/
└── agent_id/
    └── session_id/
        ├── events.lance/
        │   ├── _manifest.json
        │   └── segments/<epoch-writer>.lance/
        └── session_id.md
```

包含 subagent 的 Run：

```text
storage/
└── agent_id/
    └── root_session_id/
        ├── events.lance/          # manifest + writer segments，按 session_id 过滤
        ├── root_session_id.md
        └── agent-<id>.md
```

独立的 Storyline 分析 store 使用 `CURRENT`、`generations/<id>/{runs,steps,tool_calls}.lance`
和根级共享 `objects.lance`；它不改变上面的 canonical event 目录。

系统生成的 AgenticMD 使用 `{session_id}.md` 文件名和
`<!-- persisting:block:{source} … -->` 块结构；读取器同时接受无 speaker 的块、旧
`role/seq/session/agent` 字段和普通 Markdown 正文。

## 5. 写入与一致性

1. `EventRecord` 进入 Lance 前转换为 Arrow 行，一个有界微批对应当前 epoch segment 的一次
   Lance append，随后以 manifest CAS 发布精确 version。
2. 热路径不读取旧行、row count 或 `event_id`，不执行查重、索引、压缩或 vacuum。
3. `seq` 是 producer 定义的 Storyline 序号；replay cursor 使用不可变的物理 append 顺序。
4. Run bucket 中不同 Story 共享 manifest 和 epoch segment，但 replay/stats 按 `session_id` 隔离。
5. live Markdown 以 `call_id + source`（兼容旧 role）定位块，允许流式 agent 原地更新。
6. canonical append 与派生投影分别报告结果；投影失败不能伪装成事件已持久化。

事件事实层提供 at-least-once append，不提供 exactly-once 或 ID 唯一性。truncate、overwrite
和 retry dedup 不属于事实写路径；转换到已有 Run 会失败，裁剪应创建新 Run 或在派生
Storyline 上完成。compaction、`session_id` 索引和 vacuum 是显式离线维护。

上层 Run lease 产生单调 epoch；`EventWriterFence(epoch, writer_id)` 在新 writer 写数据前
激活。reader 只读取 manifest 固定的 segment version，因此旧 writer 在 takeover 后完成的
底层 append 不可见。相同 epoch 的另一个 writer_id 会被拒绝。该协议提供 writer fencing，
不把并发多 writer 合并定义为支持的写入模式。

## 6. 格式转换

通用外围格式通过 Storyline hub 转换：

```text
AgenticMD ─┐
ATIF ──────┼── Storyline ── events / AgenticMD / ATIF / ACTF / OpenAI messages
ACTF ──────┤
OpenAI msg ┘
```

需要保存原始 payload 的路径直接读写 events，不能经有损 Storyline roundtrip。
外围交换格式由 `pchronicle import/export` 处理。

## 7. 组件边界

| 组件 | 负责 | 不负责 |
|---|---|---|
| Gateway | 协议解析、调用生命周期、采集顺序、live projection 策略 | 通用 store、格式 schema、离线转换 |
| pChronicle | 格式、路径、落盘、读取、转换、judgment 与 revision lineage | 网络转发、Agent 生命周期 |
| pVisor | Run 生命周期及 Gateway/OverlayNet/OverlayFS 装配 | 长期轨迹 schema |

## 8. 相关文档

- [Dataset Catalog](catalog.md)
- [AgenticMD 格式](../reference/agenticmd.md)
- [Gateway 架构](../../pvisor/design/gateway.md)
- [pVisor 命令](../../pvisor/reference/cli.md)
- [`pchronicle` Dataset 命令](../reference/cli.md)
