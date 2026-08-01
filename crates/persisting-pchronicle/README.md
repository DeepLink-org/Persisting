# pChronicle

**Persisting 的 Agent 轨迹结构化存储层。**

pChronicle 统一拥有轨迹的逻辑格式、物理 schema、落盘、读取、格式转换和可重建视图。其它 crate 可以生产或消费轨迹，但不应再实现自己的轨迹格式或持久化后端。

## 组件边界

| 组件 | 负责 | 不负责 |
|---|---|---|
| `persisting-pchronicle` | `EventRecord` / `EventRow`；AgenticMD frontmatter 与文档 I/O；Lance 与 AgenticMD 后端；目录布局与分区发现；轨迹 service；回放、统计、truncate、物化、compact；judgment 规划、provider 调用与持久化；格式转换与 ATIF 规范化视图 | HTTP 代理、RPC/ABI、搜索索引 |
| `persisting-pvisor` | 管理 Agent Run/Attempt，并装配 Gateway 等运行时驱动 | 定义轨迹格式、物理 schema 或历史查询语义 |
| `persisting-gateway` | 作为 pVisor 内部驱动观察 HTTP/LLM 生命周期，产出 `EventRecord` | 成为一级 Run 管理器或定义通用存储后端 |
| `persisting-engine` | 稳定 ABI、proto adapter 和 Lance search | 实现轨迹领域逻辑或持久化 |
| `persisting-cli` | 参数解析、输入适配和输出展示 | 解析或持久化轨迹格式 |

正式边界及迁移规则见 [RFC-0003: pChronicle Ownership](../../docs/src/rfcs/0003-pchronicle-ownership.md)。

## 存储模型

```text
pVisor Gateway / import
      │ EventRecord
      ▼
events.lance                  canonical、append-only、可回放
      │
      ├──► AgenticMD          可重建的人读投影
      ├──► Storyline          ATIF-aligned 互操作 hub
      └──► normalized ATIF    sessions / steps / tool_calls 查询视图
```

- `StructuredStore` 是统一异步物理存储接口。
- `LanceEventStore` 是 canonical event log 后端。
- `AgenticMdStore` 是 AgenticMD 物理投影后端。
- `AgenticmdSessionFrontmatter`、`write_agenticmd_document`、`rewrite_agenticmd_preamble` 和 `index_agenticmd_path` 统一负责 AgenticMD 文档契约与文件操作。
- `NormalizedStore` 是派生 ATIF 三表的查询接口；旧名 `ChronicleStore` 仅为兼容别名。
- `materialize_lance_to_markdown`、`compact_markdown_to_lance` 和 `layer_stats` 统一负责层间操作。
- `StorageSelection`、`expand_story_locations` 与 `truncate_lance_session` 统一负责存储策略和维护。
- `judge_trajectory`、`JudgeRow` 及 judgment API 统一负责评测规划、provider 调用和结构化持久化；Engine 只映射 proto。
- Python `persisting.pchronicle` 是通过 `persisting._core` 调用本 crate 的兼容层，不单独实现校验、存储或视图语义。

## 格式架构

`events` 保存发生过的原始事实；`storyline` 是外围格式互操作的唯一 hub：

```text
events ──┐
agenticmd ┼──► storyline ──► events / agenticmd / openai_msg / atif
openai_msg┤
atif ─────┘
```

| 名称 | 角色 | 典型产物 |
|---|---|---|
| `events` | canonical 事实流，仅正式落盘为 Lance | `events.lance/` |
| `storyline` | ATIF-aligned 互操作 hub | `storyline.json` |
| `agenticmd` | 人读 TLV Markdown 投影 | `{session}.md` |
| `openai_msg` | OpenAI messages 外围格式 | JSON |
| `atif` | Harbor ATIF 外围格式及规范化视图 | JSON / 三表 |

字符串格式转换使用 `into_storyline`、`from_storyline`、`convert`。`events` 的 JSON/JSONL 只用于调试导出，不是正式存储格式。

## 跨组件兼容语料

pChronicle 的测试直接复用 `persisting-gateway/tests/fixtures`，而不是只依赖手工构造的最小记录：

- Capture 的真实 AgenticMD golden trajectory 必须通过严格解析、block→event 映射、RON wire、Storyline 往返以及 Lance→AgenticMD materialize；
- Capture 的 request、response、provider snapshot 和 SSE 文本语料必须在 `EventRecord`、Arrow batch 与 Lance append/replay 中无损往返；
- corpus 测试设置最小样本数量，防止 fixture 被意外缩减后测试仍静默通过。

对应测试见 `tests/capture_fixture_corpus.rs`。

## 规范

- [RFC-0001: Storyline Format](../../docs/src/rfcs/0001-storyline-format.md)
- [RFC-0002: Events Format](../../docs/src/rfcs/0002-events-format.md)
- [RFC-0003: pChronicle Ownership](../../docs/src/rfcs/0003-pchronicle-ownership.md)
