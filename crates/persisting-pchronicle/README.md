# pChronicle

**Persisting 的 Agent 轨迹结构化存储层。**

pChronicle 统一拥有轨迹的逻辑格式、物理 schema、落盘、读取、格式转换、检索和可重建视图。其它 crate 可以生产或消费轨迹，但不应再实现自己的轨迹格式或持久化后端。

## 组件边界

| 组件 | 负责 | 不负责 |
|---|---|---|
| `persisting-pchronicle` | `EventRecord` / `EventRow`；AgenticMD frontmatter 与文档 I/O；Lance 后端；目录布局与分区发现；轨迹 service；回放、统计、物化、judgment、格式转换、Search 和 ATIF 查询 | HTTP 代理、Agent 生命周期 |
| `persisting-pvisor` | 管理 Agent Run/Attempt，并装配 Gateway 等运行时驱动 | 定义轨迹格式、物理 schema 或历史查询语义 |
| `persisting-gateway` | 作为 pVisor 内部驱动观察 HTTP/LLM 生命周期，产出 `EventRecord` | 成为一级 Run 管理器或定义通用存储后端 |
| `persisting-cli` | 参数解析、输入适配和输出展示 | 解析或持久化轨迹格式 |

正式边界及迁移规则见 [RFC-0003: pChronicle Ownership](../../docs/src/rfcs/0003-pchronicle-ownership.md)。

## 存储模型

```text
pVisor lifecycle / Gateway / import
      │ EventRecord
      ▼
events.lance                  canonical、append-only、可回放
      │
      ├──► AgenticMD          可重建的人读投影
      └──► Storyline          ATIF-aligned 互操作 hub / 三表 Lance

judgments.lance               规范化派生评测，不修改 canonical event schema
```

- `StructuredStore` 是 canonical event log 的异步存储接口。
- `RawEventLanceStore` 是 canonical event log 后端。
- `RawEventLanceAppender` 为在线 capture 缓存当前 writer epoch 的私有 Lance segment；pVisor
  writer 用 2 ms / 256 条的有界窗口合并为一次 Lance append，再通过一个小型 manifest CAS
  发布该 segment 的精确 version。热路径不查旧行、不查重、不建索引、不压缩，writer 结束
  也不会同步执行 fragment/index/vacuum 维护。
- canonical event log 是 at-least-once、严格 append-only 的事实层。`event_id` 是原样保留的
  业务字段，不具备存储唯一性；重复 ID 和重试产生的重复行都会持久化，由下游业务投影
  决定是否去重。未提供 ID 时物理列写入 `NULL`，pChronicle 不生成业务身份。
- `RawEventLanceAppender::fenced(EventWriterFence)` 将 Run lease epoch 带入存储提交协议。
  新 epoch 先通过 conditional manifest update 激活；旧 writer 后续 Lance commit 只能形成
  不可见版本，无法更新 manifest。自动 writer 也会原子领取递增 epoch。
- compaction、`session_id` 索引和 vacuum 仅由显式 `maintain` 执行，不阻塞 capture 关闭。
- `RawEventLanceStore::replay_available` 每次读取一个原子 manifest revision，并 checkout
  其中固定的 segment versions；manifest 尚未创建时返回 `None`，供
  `persisting query follow` 从 offset 连续消费已发布的 event micro-batch。
- AgenticMD 是从 canonical events 或 Storyline 生成的可丢弃人读/调试视图，不是存储后端。
- `StorylineLanceStore` 将 Storyline 原子提交为 `runs.lance`、`steps.lance`、`tool_calls.lance` 三张规范化表；超过阈值的 JSON/UTF-8 单元按 BLAKE3 寻址、跨轨迹去重并写入共享 `objects.lance` 的 Lance Blob v2，三表 schema 不变。
- `StorylineLanceStore::get_storylines` 对三张表各读取一次同一 generation 快照，支持
  pPilot 批量重建多条 Storyline，并保持请求顺序。
- `StorylineDataSource` 将同一 generation 的三张表注册到 DataFusion，并下推列裁剪、谓词、limit 和标量索引查询；只有查询实际引用内容列时才读取并恢复 Blob，内部引用不会出现在 SQL 结果中。
- `AtifDataSource::open` 与 OpenAI/ACTF 共用按文件 lazy 的 DataFusion datasource；文件由
  manifest 大小/身份校验、并发闸门和共享有界 LRU 管理。显式传入完整内存值的
  `from_json` / `from_trajectories` 保留 `MemTable`。
- `ChronicleQueryEngine` 对 Lance、ATIF、OpenAI JSON 与 ACTF 暴露同一套 `runs`、`steps`、
  `tool_calls` SQL 和 Arrow/JSONL 结果 API。三种文件输入的临时查询表额外带
  `_file_` 相对路径列，支持 `=`、`IN`、`LIKE` 文件级裁剪；每个命中文件作为一个 lazy
  streaming partition 打开，该列不属于 Lance 三表 schema。多文件内建表 join 必须把
  `_file_` 纳入 join key。
- ATIF compact JSON/JSONL 的 `steps` 查询在 `TableProvider::scan` 边界接收 DataFusion
  projection 和安全谓词，直接跳过未引用的大字段、按 `session_id`/`step_id`/`source`
  提前裁剪并生成 projected Arrow batch，不经过 Storyline 导入和三表全量构造。
  该路径校验 JSON、必需字段和当前表内约束；跨表引用完整性仍由导入路径或完整规范化
  fallback 校验。
  `SELECT *`、JSON 数组/pretty JSON、其他表和 OpenAI/ACTF 保留完整规范化 fallback。
- `RunControlStore` 以单个 CAS record 管理 Run lease epoch 与 immutable terminal `RunCommit`。
- `AttemptRegistry` 以 lease epoch fence pVisor Attempt 的注册、心跳和完整终态结果，供 pPilot 重启后收敛。
- `AgenticmdSessionFrontmatter`、`write_agenticmd_document`、`rewrite_agenticmd_preamble` 和 `index_agenticmd_path` 负责宽松的 AgenticMD 可视化与调试文件操作。
- `materialize_lance_to_markdown` 单向重建 AgenticMD；`layer_stats` 仅把 Markdown 块数作为诊断信息。
- `expand_story_locations` 负责 canonical 存储发现。事实层拒绝 truncate/overwrite；需要裁剪
  时创建新 Run 或在 Storyline 派生层表达。
- `judge_trajectory`、`JudgeRow` 及 judgment API 统一负责评测规划、provider 调用，并按
  `(session_id, call_id, rubric_id)` upsert 到独立的 `judgments.lance`。
- `search` 模块统一负责 Lance 文档写入、IVF-PQ/FTS 索引与检索。

Storyline 的 `runs`、`steps`、`tool_calls` 是唯一的规范化三表 schema。旧的 ATIF
`sessions`、`steps`、`tool_calls` 内存表及 Python 门面已经移除；ATIF 输入统一先转换为
Storyline，再进入相同的 Arrow/DataFusion 查询模型。

轨迹 append、replay 和 stats 只使用 Lance。Markdown 输入仍可通过显式格式导入，但会
解析后写入 canonical Lance；系统不会把已有 `.md` 自动选作存储层，也不会从调试视图
反向 compact 覆盖事实事件。

Storyline 导入和替换以最多 256 条轨迹为一个有界批次推进三张表和共享内容表；读取
`CURRENT` 后每张 dataset 固定到同一版本元组。频繁替换达到 fragment 阈值会自动 compact，长期
维护可显式运行：

```bash
ppilot chronicle maintain ./storyline-store --vacuum-retention-hours 168
```

该命令补齐并刷新标量索引、合并小 fragment，并在新三表版本已经通过 `CURRENT` 原子
可见后回收超过保留期的旧版本。

`ppilot chronicle import` 默认自动识别 ATIF、ACTF 与 OpenAI-message 输入。ATIF 的 NDJSON
逐行解析，目录按稳定顺序逐文件消费；OpenAI-message 输入额外支持包含多个 session 的
裸 step 数组。空 ATIF store 只规范化每条 trajectory 一次，再通过三条有界 Arrow
channel 并行创建 `runs`、`steps`、`tool_calls`；其它导入以最多 256 个 Storyline 为一批
执行增量替换。所有路径都只在全部输入成功后更新一次 `CURRENT`。

OpenAI corpus 导入会在现有 `extra_json` 列保留带版本的原 row、源文件分组和 ordinal，
因此可在不改变三表 schema 的前提下恢复 JSON 数据模型：

```bash
ppilot chronicle import ./openai-data ./storyline-store --format openai_msg
ppilot chronicle export ./storyline-store ./recovered --format openai_msg
ppilot convert ./openai-data ./storyline-store --to lance
ppilot convert ./storyline-store ./recovered --from lance --to openai_msg
```

恢复保证键值、null、嵌套值和数组顺序，不保证原文件空白或对象键顺序。缺失保真元数据
时 export 会失败，而不会静默输出有损近似值。

ACTF v1.0 同样不修改三表 schema：每个 attempt 映射为一条 Storyline，根、attempt、
trajectory 和原始 step 保存在现有 `extra_json` 中。经三表 Lance 恢复后可保持 JSON
数据模型级无损：

```bash
ppilot convert ./task.actf.json ./storyline-store --to lance
ppilot convert ./storyline-store ./recovered --from lance --to actf
```

### S3 对象存储

`RunControlStore::open` 同样接受本地路径或 `s3://` 等对象存储 URI。本地更新使用
per-Run 文件锁、`fsync` 和原子 rename；对象存储更新使用 create-if-absent 或带
ETag/version precondition 的 conditional update。lease 与 commit 位于同一 control
object，避免“检查旧 lease 后、写 commit 前”被新 epoch 穿透的 TOCTOU 窗口。

未过期 lease 不允许其他 owner 普通获取；reconciler 确认原 attempt 不存在后才调用
显式 takeover 并递增 epoch。terminal commit 只接受当前 epoch，相同请求可幂等重放，
不同 attempt/digest 返回 conflict。

canonical `RawEventLanceStore` 和规范化 `StorylineLanceStore` 都接受
`s3://bucket/prefix`。前者把每个 Run 的 fencing manifest 和 writer segments 写到
`<prefix>/<agent>/<run>/events.lance`；后者把版本化三表 dataset 与原子可见的
`CURRENT` 快照版本元组放在同一个对象存储前缀下。本地路径 API 保持兼容：

```rust,no_run
# async fn example(stories: &[persisting_pchronicle::StorylineDocument]) -> anyhow::Result<()> {
use persisting_pchronicle::StorylineLanceStore;

let store = StorylineLanceStore::open_uri(
    "s3://trajectory-bucket/persisting/storylines"
).await?;
store.replace_storylines(stories).await?;
# Ok(())
# }
```

S3 凭证不作为 Persisting 参数传递或写入日志，使用 AWS 标准凭证链：

```bash
export AWS_REGION=us-east-1
export AWS_ACCESS_KEY_ID=...
export AWS_SECRET_ACCESS_KEY=...
# 临时凭证还需 AWS_SESSION_TOKEN
```

MinIO 等 S3-compatible 服务另外设置 `AWS_ENDPOINT`、`AWS_DEFAULT_REGION`；HTTP
端点还需 `AWS_ALLOW_HTTP=true`。同一 Run 的 writer ownership 由 manifest 中的
`(epoch, writer_id)` fencing token 和 ETag/version conditional update 强制执行；不同 Run
使用独立 `events.lance` 前缀并行生产。底层提交失败或失效 writer 可能留下不可达 Lance
version，显式 maintenance 和 bucket 生命周期规则负责回收。

真实 S3/MinIO 契约测试默认忽略，可在有隔离测试前缀的环境中显式运行：

```bash
PCHRONICLE_S3_TEST_URI=s3://bucket/test-prefix \
  cargo test -p persisting-pchronicle --test s3_storage -- --ignored
```

## 格式架构

`events` 保存发生过的原始事实；`storyline` 是外围格式互操作的唯一 hub：

```text
events ──┐
agenticmd ┼──► storyline ──► events / agenticmd / openai_msg / atif / actf
openai_msg┤
atif ─────┤
actf ─────┘
```

| 名称 | 角色 | 典型产物 |
|---|---|---|
| `events` | canonical 事实流，仅正式落盘为 Lance | `events.lance/` |
| `storyline` | ATIF-aligned 互操作 hub | `storyline.json` |
| `agenticmd` | 宽松的人读/调试 Markdown 视图 | `{session}.md` |
| `openai_msg` | OpenAI messages 外围格式 | JSON |
| `atif` | Harbor ATIF 外围格式及规范化视图 | JSON / JSONL |
| `actf` | ACTF v1.0 task/attempt 外围格式 | JSON |

字符串格式转换使用 `into_storyline`、`from_storyline`、`convert`。`events` 的 JSON/JSONL 只用于调试导出，不是正式存储格式。

## 跨组件测试语料

pChronicle 的测试直接复用 `persisting-gateway/tests/fixtures`，而不是只依赖手工构造的最小记录：

- Capture 的真实 AgenticMD golden trajectory 必须通过宽松解析、显式导入映射、Storyline 往返以及 Lance→AgenticMD materialize；
- Capture 的 request、response、provider snapshot 和 SSE 文本语料必须在 `EventRecord`、Arrow batch 与 Lance append/replay 中无损往返；
- corpus 测试设置最小样本数量，防止 fixture 被意外缩减后测试仍静默通过。

对应测试见 `tests/capture_fixture_corpus.rs`。

独立的 ATIF corpus 位于 `tests/fixtures/atif/`，包含 8 条 10–20 step 的确定性轨迹，
供格式转换、Storyline 三表和空间占用测试共同使用：

```bash
cargo test -p persisting-pchronicle --test atif_lance_corpus
cargo bench -p persisting-pchronicle --bench atif_storyline_lance
PCHRONICLE_BENCH_SCALE=128 cargo bench -p persisting-pchronicle --bench lance_vs_json
just examples-pchronicle  # 包含 point / batch / live follow 的可复现产品 CLI 对比
```

可通过 `PCHRONICLE_BENCH_ITERS` 调整转换 benchmark 的重复次数。

## 规范

- [RFC-0001: Storyline Format](../../docs/src/rfcs/0001-storyline-format.md)
- [RFC-0002: Events Format](../../docs/src/rfcs/0002-events-format.md)
- [RFC-0003: pChronicle Ownership](../../docs/src/rfcs/0003-pchronicle-ownership.md)
- [RFC-0004: ACTF v1.0](../../docs/src/rfcs/0004-actf-format.md)
