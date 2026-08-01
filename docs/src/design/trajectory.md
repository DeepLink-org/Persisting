# pChronicle 轨迹存储

> 当前实现说明。规范性所有权见 [RFC-0003](../rfcs/0003-pchronicle-ownership.md)，
> 命令见 [`persisting traj`](cli-traj.md)。

## 1. 定位

`persisting-pchronicle` 是 Agent 轨迹的结构化存储层，统一拥有：

- `EventRecord` 逻辑事件和 Lance `EventRow` schema；
- Run / Story 坐标、目录布局和发现规则；
- Lance 与 AgenticMD 的读写、统计和维护；
- events、Storyline、ATIF、OpenAI messages、AgenticMD 之间的格式转换；
- materialize、judgment 和标准查询视图。

Gateway 负责把协议流量解释为事件；Engine 只提供动态 ABI 和 RPC 适配；CLI 只
解析参数与展示结果。三者都不定义第二套轨迹落盘格式。

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

`events.lance/` 是完整事件表示，保留 HTTP/模型调用、时间、身份、payload 和顺序，
适合回放、统计、评测和派生数据。需要审计保真度的工作流应使用这一层。

### AgenticMD

AgenticMD 是面向人的结构化 Markdown。它保存可见对话块和会话摘要，适合实时查看、
代码审阅与人工分析。它会省略协议噪声，因此不是原始 HTTP 事件的无损替代。

两者不是要求同步双写的两份事实源：

- `pvisor run --chronicle-mode lance` 写 Lance；
- `pvisor run --gateway-stream-markdown` 直接维护 AgenticMD，适合轻量本地使用；
- `traj materialize` 从 Lance 重建 AgenticMD；
- `traj add` 根据显式选择或已有层写入一个存储层；
- `traj replay --storage-format auto` 优先读取 Lance，只有 Markdown 时读取 Markdown。

## 4. 目录布局

扁平 Story：

```text
storage/
└── agent_id/
    └── session_id/
        ├── events.lance/
        └── session_id.md
```

包含 subagent 的 Run：

```text
storage/
└── agent_id/
    └── root_session_id/
        ├── events.lance/          # 按 session_id 分区
        ├── root_session_id.md
        └── agent-<id>.md
```

读取器仍能识别早期的 `0001.md` 和 `trajectory.tlv.md`，但新写入使用
`{session_id}.md`。这些名称只是历史数据读取规则，不是新的写入选项。

## 5. 写入与一致性

1. `EventRecord` 进入 Lance 前转换为稳定 Arrow 行。
2. 同一 dataset 内分配单调 `seq`；当前进程内写入串行化。
3. Run bucket 中不同 Story 共享 dataset，但 replay/stats 按 `session_id` 隔离。
4. live Markdown 以 `call_id + speaker` 定位块，允许流式 assistant 原地更新。
5. canonical append 与派生投影分别报告结果；投影失败不能伪装成事件已持久化。

跨进程多 writer 仍需要上层单 writer/租约约束；当前实现不宣称提供分布式 CAS。

## 6. 格式转换

通用外围格式通过 Storyline hub 转换：

```text
AgenticMD ─┐
ATIF ──────┼── Storyline ── events / AgenticMD / ATIF / OpenAI messages
OpenAI msg ┘
```

需要保存原始 payload 的路径直接读写 events，不能经有损 Storyline roundtrip。
`traj convert` 用于文件格式转换；`traj materialize` 专门处理 Lance → AgenticMD。

## 7. 组件边界

| 组件 | 负责 | 不负责 |
|---|---|---|
| Gateway | 协议解析、调用生命周期、采集顺序、live projection 策略 | 通用 store、格式 schema、离线转换 |
| pChronicle | 格式、路径、落盘、读取、转换、judgment | 网络转发、Agent 生命周期 |
| Engine | Search 实现、稳定 ABI、trajectory RPC 适配 | 轨迹领域实现 |
| pVisor | Run 生命周期及 Gateway/OverlayNet/OverlayFS 装配 | 长期轨迹 schema |

## 8. 相关文档

- [AgenticMD 格式](trajectory-format.md)
- [Gateway 架构](gateway.md)
- [pVisor 命令](cli-pvisor.md)
- [Traj 命令](cli-traj.md)
