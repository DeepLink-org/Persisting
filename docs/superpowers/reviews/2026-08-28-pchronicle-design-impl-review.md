# pChronicle 设计与实现整体 Review

## Status

| 项 | 内容 |
|---|---|
| 评审时间 | 2026-08-28 |
| 覆盖范围 | `crates/persisting-pchronicle`（约 51k 行 Rust / 99 个源文件）全部非 `search` 源码，含 `README.md` 与 `docs/src/rfcs/0003-pchronicle-ownership.md` 的对照核实 |
| 排除范围 | `src/search/`、`src/operations/`（search 适配层）按项目默认约定排除；`persisting-dlcapt` 不在范围 |
| 评审方式 | 五个子系统并行探查后交叉核对，对关键断言逐条读原文核实（见文末「核实清单」） |
| 关联消费者 | `persisting-pchronicle-cli`、`persisting-gateway`（pPilot / pVisor 通过 spawn `pchronicle` 二进制消费，不链接库） |

## 结论摘要

pChronicle 的核心设计是对的，而且好得超出预期。真正的债务不在概念层，而在**执行一致性**：对外承诺的能力位（streaming、filter exact）和预算（unknown fields limits、查询行数）有相当一部分没有在实现里落地；README 描述的四模块门面没有编译约束；几个核心模块已经膨胀到 2000 行量级。这不是一个需要重新设计的系统，而是一个需要把已经写在文档里的承诺补齐、然后拆文件的系统。

三个最重要的判断：

1. **单枢纽格式转换 + epoch-fenced 事实层 + CURRENT 原子发布 + watermark 投影，这四件事都做扎实了**，还配了真实竞争的并发测试。这是这个 crate 值得保留的资产，任何重构都不该破坏它们。
2. **唯一能被外部输入直接触发的资源耗尽路径是 SQL 查询入口**：`query()` / `query_jsonl()` 全量 collect 且默认无内存上限。有预算的流式路径已经写好了，只是便捷 API 绕过了它。
3. **README 写得非常具体（承诺了无损边界、能力矩阵、预算行为、门面结构），这是优点**——但具体的承诺一旦漂移，代价比含糊的文档更大，因为下游会当契约用。目前有 5 处明确漂移。

## 现状盘点

### 分层评分

设计 = 抽象与边界是否成立；实现 = 代码是否兑现了该抽象。

| 子系统 | 设计 | 实现 | 最该关注的一件事 |
|---|:---:|:---:|---|
| 格式转换枢纽 `formats/` + `convert/` | A | B | capabilities 里的 `streaming_input` 与 codec 内部无界 unknown limits 都不是实话 |
| unknown fields 保真 | A | A− | 机制本身可证明；缺 proptest（最复杂的不变量只有单测） |
| 事实层 `events.lance` + manifest | A | A− | `FilterPushdown::Exact` 无条件上报；replay 两趟 scan |
| Storyline Lance + CURRENT | A− | B | `mod.rs` 已 1940 行；local CURRENT 写入不做 etag 比较，跨进程只靠 flock |
| 投影管线 `projection/` | A− | B+ | watermark / lineage 逻辑扎实，但缺 kill -9 中途 sync 的恢复测试 |
| 写入队列 `append_queue` | B+ | B | `in_flight` 语义错位 + 忙等；maintenance channel 满时静默丢工作 |
| 查询引擎 `query_engine` | B+ | C+ | 只读门禁与 `_file_` join 守卫是好设计，但便捷 API 无预算、默认无内存上限 |
| AgenticMD 编解码 | B+ | B | byte-span upsert 很漂亮；无 `length` 时的 marker 截断是唯一真 fragility |
| `layout/` + `resolve` 路径推断 | C+ | C+ | 800 行启发式，行为由文件系统驱动，无性质测试 |
| 公共 API 门面 | B | C | 四模块是文档意图，不是编译约束；CLI 深度绑定 storage 全表面 |

### 已建成的设计资产

| 资产 | 位置 | 说明 |
|---|---|---|
| 单枢纽格式转换 | `convert/mod.rs:1-12` | 所有外围格式只与 `StorylineDocument` 互转，转换复杂度 O(N) 而非格式两两组合的 O(N²)。`TrajectoryFormat` trait + 静态 registry 让新增 codec 只需 impl + 注册 |
| unknown fields 无损机制 | `formats/unknown_fields.rs:362-401`、`815-817` | RFC 6901 JSON Pointer + 按源格式命名空间 + version-1 `_storyline` envelope + 冲突一律 fail closed。跨格式多跳有集成测试 |
| 事实层 epoch fencing | `store/events/manifest.rs:187-251`、`472-477` | writer 先写私有 segment 再做 epoch-fenced CAS 发布；崩溃在发布前只留不可达的 Lance 版本，读者 pin manifest 完全免受影响 |
| CURRENT 原子发布 | `store/storyline/mod.rs:943-975`、`991-1022` | 三表 + objects 版本全部就绪才移动 CURRENT；失败释放 lease 并删除未提交 generation。读者永远看到完整快照 |
| 投影 lineage 保守优先 | `projection/storyline.rs:430-432`、`237-240`、`271-274` | watermark 用 `fact_version` / `fact_rows`，纯 compaction 的 `layout_revision` 变化不算 stale；recipe 变更或 watermark 非单调时返回 `RequiresRebuild` 而不是猜 |
| content offload + 延迟物化 | `store/storyline/datafusion.rs:191-200` | 大 payload 进 `objects.lance`，表内只留 preview/ref，查询走 `ContentHydrationExec`。这一层最实在的性能设计 |
| 时间戳保留源形态 | `formats/timestamp.rs:7-17` | `StorylineTimestamp` 同时保留 wire scalar 和 canonical instant，拒绝亚纳秒精度。比统一转 RFC3339 字符串高明一档 |
| bounded 流式解析 | `formats/common/json_stream.rs:77-196`、`205-291` | 手写 depth / string escape 跟踪 + 复用 buffer 的 `read_bounded_json_object`，安全与性能兼顾，有 proptest |
| analysis 编译器分层正确 | `analysis_compile.rs:4-5`、`196-379` | `AnalysisSpec` → 只读 SQL，白名单 intent / grain / measure / dimension，明确不碰 DataFusion 执行 |
| 测试约定写进文档并被遵守 | `tests/README.md:52-62`、`store/events/manifest.rs:777-801` | 明确禁止「用进程级 `root_write_lock` 把并发测试串行化后宣称覆盖了 CAS」，要求断言精确计数、回归测试先证明 bug；有 32-writer CAS 竞争测试 |

## Review 发现

### P0 —— 正确性与资源，建议尽快处理

| ID | 问题 | 位置 | 影响 |
|---|---|---|---|
| P0-1 | SQL 查询入口无行/字节预算，默认无内存上限 | `store/query_engine.rs:348-370`（`query` / `query_jsonl` 全量 collect）、`442-481`（`memory_limit_bytes` 默认 `None`） | 对外暴露 SQL 等于暴露 DoS 面。流式有预算的路径 `write_query_jsonl_bounded` 已实现且 CLI 在用，但便捷 API 无任何上限，也没有 query timeout |
| P0-2 | unknown fields 预算没有贯通到各 codec | `formats/atif.rs:328`、`formats/actf/mod.rs:173`、`formats/openai_corpus.rs:230` 硬编码 `UnknownFieldLimits::default()`（= `usize::MAX`）；`document.rs:76` 在 decode 之后才校验 `DocumentCodecOptions` | 畸形输入的超大 unknown payload 会先完整进内存，最后才可能被拒。README 承诺的「显式配置的有限上限」在 codec 内部拿不到 |
| P0-3 | 解码路径存在静默数据丢失 | `formats/codex.rs:145`、`formats/claude_code.rs:148`（`from_rfc3339(..).ok()` 丢弃非法时间戳）；`formats/common/jsonl.rs:99`（JSON 解析失败降级为整段 string）、`:49`（multimodal 数组拼成单字符串） | 与 crate 其他部分一贯的 fail-closed 风格直接矛盾。事实层丢时间戳 / 丢结构且不报错，下游无从发现 |
| P0-4 | 每次调用新建 Tokio runtime | `discovery.rs:95-100` `expand_story_locations_blocking` | 在已有 runtime 的线程上调用会嵌套 runtime（panic）或阻塞 worker；每次调用还付一次多线程 runtime 构建成本 |

### P1 —— 设计一致性与可维护性

| ID | 问题 | 位置 | 影响 |
|---|---|---|---|
| P1-1 | 四模块公共门面只是文档意图，不是编译约束 | `lib.rs:31` `pub mod analysis_compile`；`lib.rs:71-81` search feature 下 `pub use messages::*` / `operations::*`；`tests/public_api.rs` 只是正向编译测试 | README 声称「默认功能面只通过四个模块组织」，实际默认就有 5 个公开模块，search 开启后 6 个 + 一批 crate 根 re-export。没有 `compile_fail` 守卫，回归无法拦住 |
| P1-2 | capabilities 上报与实现脱节 | `formats/actf/mod.rs:42-48` 声明 `streaming_input: true` 但 `:145-153` 是 `read_to_string` 全量读（Storyline / OpenAI 同）；`store/events/datafusion.rs:124-127` 对任意 `Expr` 无条件返回 `FilterPushdown::Exact` | README 把「能力由实际打开的 `DocumentSource` 报告，不按格式名推断」当设计卖点，但报告值本身有部分不可信。Exact 是否成立取决于 Lance 对该 Expr 的完整求值，缺验证测试 |
| P1-3 | 多个模块已超过可维护体量 | `store/storyline/mod.rs` ≈1940 行（`replace_storyline_stream_with_projection` 单函数 ~330 行）、`formats/actf/convert.rs` ≈1961、`formats/openai_corpus.rs` ≈1915、`store/catalog/mod.rs` ≈920、`store/events/mod.rs` ≈1095、`layout/resolve.rs` ≈824 | 读写路径、DataFusion 适配、lease、maintain 混在同一文件，改动半径大，评审成本高 |
| P1-4 | append 队列的关停语义靠隐式顺序 + 忙等 | `append_queue.rs:132-151`（`in_flight` 只覆盖 `try_send` 窗口）、`:166-168` 与 `:535-537`（`yield_now` 自旋）；`:490-493`（maintenance channel 满时只 warn） | `in_flight` 名字暗示「未完成的工作量」，实际不表示队列深度；`finish()` 等到 0 也不代表队列已空，正确性依赖 `Finish` 消息顺序而非计数。compaction 可能长期滞后 |
| P1-5 | AgenticMD 缺 `length` 时按 marker 子串截断 body | `agenticmd/codec.rs:249-253`；`docs/src/pchronicle/reference/agenticmd.md:25-27` 明确允许省略 length | 手工编辑过的文件若正文含 `<!-- persisting:block:` 会被静默截断，且无测试覆盖这条路径。有 `length` 时行为正确 |
| P1-6 | control Outcome 类型在两个 crate 重复定义 | `persisting-events/src/control.rs:31-38` 与 `store/run_control.rs:16-27` 同名 `LeaseAcquireOutcome` / `CommitRunOutcome` | CLI `control.rs:249-279` 必须手工映射两套同名类型，是典型的用错类型的温床 |
| P1-7 | async 上下文里的同步文件 IO | `store/document_source.rs:63`（async fn 内 `std::fs::read_to_string`）、`:405`（`std::fs::read`）；`store/catalog/discovery.rs:426-551`（同步 `read_dir` 递归，在 `discover().await` 热路径上） | 阻塞 runtime worker；目录规模大时 discover 会拖住整个 executor |

### P2 —— 债务，可排期

| ID | 问题 | 位置 | 影响 |
|---|---|---|---|
| P2-1 | Storyline 领域模型大量 stringly-typed 字段 | `formats/storyline.rs:133`（`turn.source: String`，validate 只查非空）、`:170-188`（`kind` 靠启发式推断）、`:109-114`（`relation` 默认 `"spawn"` 的 String）、`:75-76`（`origin.format` 是 String 而非 `DocumentFormat`） | hub 模型是所有格式的唯一枢纽，不变量却没编码进类型，只能靠 imperative validate 兜 |
| P2-2 | 格式指纹探测复制 6 份 | `formats/atif.rs:192-215`、`actf/mod.rs:115-138`、`codex.rs:87-114`、`claude_code.rs`、`storyline.rs`、`openai_corpus.rs` | 模式完全相同（UTF-8 → trim → 解析首对象或前 32 行）；改一条识别规则要改 6 处 |
| P2-3 | 读路径的 N+1 IO | `store/events/mod.rs:986-1001`（`replay_available` 每 segment 先 `count_rows` 再读）、`:874-894`（`distinct_session_ids_in_run` 每 segment 全 scan）；`store/document_source.rs:439-466`（每 document 3 次 SQL） | segment 数量增长后 replay 与 discovery 成本线性放大 |
| P2-4 | 本地原子写实现了 4 份 | `store/cas_store.rs:149-170`、`store/events/manifest.rs:615-627`、`store/storyline/mod.rs:1531-1558`，外加 revision 走 Lance 事务 | tmp + fsync + rename + dir fsync 的同一模式没抽公共 helper，任一处漏 fsync 不易发现 |
| P2-5 | `s3-store` 是默认 feature | `Cargo.toml:11`（`default = ["lance-store", "s3-store"]`），注释自己说是为兼容保留 | workspace 内消费者都写了 `default-features = false`，卫生良好；但外部直接依赖会静默拉进 AWS SDK 链（约多 400 个 transitive 包） |
| P2-6 | 路径推断是 800 行启发式，无性质测试 | `layout/resolve.rs`（`list_story_read_locations` 等）、`agenticmd/layout.rs:179-189`（`stem.contains('-') && len >= 8` 判定 trajectory md） | 行为由文件系统存在性驱动，难以形式化；跨版本 layout 变更时最先出问题的地方 |

### 文档与代码漂移

单独列出来是因为 pChronicle 的 README 承诺得非常具体，而具体承诺的漂移会被下游当契约用。

| README 的声明 | 位置 | 代码实际情况 |
|---|---|---|
| 「六种磁盘格式」 | `README.md:26-35` | `DocumentFormat` 有 9 个变体（`format.rs:11-30`）。表里把 `Storyline` 映射为三表 Lance，但 enum 中 `Storyline`（JSON wire）与 `StorylineLance` 是两个不同变体；`Codex` / `ClaudeCode` 完全不在表内 |
| 「默认功能面只通过四个模块组织」 | `README.md:95-106` | 默认 5 个公开模块（多 `analysis_compile`），search 开启后 6 个 + crate 根 re-export（`lib.rs:31`、`:71-81`） |
| ATIF / ACTF「streaming decode = 是」 | `README.md:80-87` | ATIF 确实是 bounded streaming（`json_stream.rs` + `atif.rs:89-112`）；ACTF 是 `read_to_string` 全量读（`actf/mod.rs:145-153`），能力位与实现不符 |
| Canonical Event「filter = exact」 | `README.md:82` | provider 对任意 `Expr` 无条件返回 `Exact`（`store/events/datafusion.rs:124-127`），既无白名单校验也无正确性测试 |
| 「显式配置的有限上限仍会在溢出时拒绝整条 Storyline」 | `README.md:64-66` | codec 内部用的是硬编码 default（无界）；配置值只在 decode 之后的 `document.rs:76` 生效，承诺在时序上晚了一步 |

## 行动项

| ID | 优先级 | 动作 | 验收标准 |
|---|---|---|---|
| A-1 | P0 | 给 `query()` / `query_jsonl()` 加默认行上限，或标记 deprecated 只保留 bounded 版本；为 `ChronicleQueryExecutionOptions` 设一个非 `None` 的默认内存上限 | 存在一个测试：构造超出默认预算的结果集，`query()` 返回失败或 `LimitExceeded` 而非 OOM；`ChronicleQueryExecutionOptions::default()` 的 `memory_limit_bytes` 非 `None` |
| A-2 | P0 | 把 `DocumentCodecOptions.unknown_fields` 透传进各 format 的 `attach_carried_unknown_fields` | ATIF / ACTF / OpenAI 三个 codec 各有一个测试：配置有限 limits 后，超限输入在 decode **期间**被拒，且峰值内存不随 payload 线性增长 |
| A-3 | P0 | 把 `codex` / `claude_code` 的时间戳 `.ok()` 与 `jsonl.rs` 的 JSON 降级改为 `InputIssue`，或至少计入 `DecodeReport` 的 warning 计数 | 有 negative test：非法 RFC3339 时间戳与非法内嵌 JSON 分别产生可观测的 issue / warning，不再静默变 `None` 或 String |
| A-4 | P0 | 删除 `expand_story_locations_blocking`，改为要求调用方提供 runtime | `discovery.rs` 中不再出现 `Runtime::Builder`；全部调用点改为 async 或由调用方 `block_on` |
| A-5 | P1 | 把 events provider 的 `FilterPushdown` 降为 `ExpressionDependent`，或实现白名单 + 正确性测试；把 ACTF / Storyline / OpenAI 的 `streaming_input` 改成实话 | 有测试对同一数据集比较「provider pushdown 结果」与「DataFusion 全量过滤结果」在复杂 Expr（`OR`、函数调用、`IS NULL`）下一致；capabilities 表与实现逐项对齐 |
| A-6 | P1 | 拆分 `store/storyline/mod.rs`（读 / 写 / DataFusion / lease 四份）与 `formats` 里两个 1900 行模块 | 单文件不超过约 800 行；`replace_storyline_stream_with_projection` 拆为可单独测试的阶段函数 |
| A-7 | P1 | 决定门面立场：要么把 `analysis_compile` 降为 `pub(crate)` / 移进 CLI 并加 `compile_fail` 守卫，要么改 README 承认 `storage` 暴露的是完整 store 表面 | README 与 `lib.rs` 的公开模块集合一致；若保留四模块承诺，则有 `compile_fail` 测试断言第五个模块不可达 |
| A-8 | P1 | 用明确的队列深度计数或 condvar 替换 `in_flight` + `yield_now`；maintenance channel 满时改为可观测的 metric 而非仅 warn | `finish()` 不再自旋；有测试断言 `finish()` 返回后队列确实已空；maintenance 积压可被 introspect 查询到 |
| A-9 | P1 | AgenticMD 缺 `length` 的场景：要么在 parse 时拒绝，要么把文档降级为「仅系统生成文件保证可解析」，两种选择都要加回归测试 | 存在覆盖「无 length 且 body 含 `BLOCK_MARKER`」的测试，行为与文档一致 |
| A-10 | P1 | 统一 control Outcome 类型：`store/run_control.rs` re-export `persisting-events` 的类型或反之 | 仓库内不存在两个同名 `LeaseAcquireOutcome` / `CommitRunOutcome`；CLI 不再需要手工映射 |
| A-11 | P1 | `document_source` 与 `catalog/discovery` 的同步 IO 改为 `spawn_blocking` 或异步 IO | async fn 内不再出现 `std::fs::read*` / `read_dir` |
| A-12 | P2 | 抽取共享的格式指纹探测 helper 与本地原子写 helper | 指纹逻辑单点定义；`tmp + fsync + rename + dir fsync` 单点定义并被四处调用点复用 |
| A-13 | P2 | 收紧 Storyline 的 `source` / `kind` / `relation` / `origin.format` 为 enum 或 validated newtype | `validate()` 能拒绝非法 `source` 值；`origin.format` 类型为 `DocumentFormat` |
| A-14 | P2 | 补 `unknown_fields`、`formats/storyline`、`layout/resolve` 的 proptest | 三个模块各有生成式测试；`tests/README.md` 的批次列表更新 |

## 遗留与风险

### 已知未解决

| 项 | 说明 |
|---|---|
| 对象存储多写者契约缺集成验证 | `store/dataset_write_lock.rs:24-27` 明确声明对象存储上是单写者契约，Lance dataset 本身没有分布式锁，只靠 manifest / CURRENT CAS 挡可见性发布——segment 级 orphan 数据仍可能堆积。`tests/s3_storage.rs` 默认 `--ignored`。若生产允许多 Gateway 写同一 run bucket，应把其中一部分提升为门禁 |
| local CURRENT 写入不做 etag 比较 | `store/storyline/writer_control.rs:333-335` 直接 overwrite，跨进程安全完全依赖 `.storyline-write.lock`；绕过 store API 手改 `CURRENT` 无 CAS 保护 |
| 缺 crash-in-the-middle 恢复测试 | 增量 sync 在三表之间崩溃时，直接读 Lance 中间 version 可能不一致（走 `CURRENT` 的读者不受影响）。逻辑上幂等可重试，但没有 kill -9 类混沌测试证明 |
| lease 过期依赖 wall clock | `store/run_control.rs:96` 用 `unix_now_ms()`，多节点时钟漂移会导致提前 / 延后 takeover，无边界校正 |
| `index_build_gate` 与 `dataset_write_lock` 零 / 近零独立测试 | 前者是内存保护而非正确性 gate，后者仅通过 revision / storyline maintain 间接覆盖 |
| `maintain()` 会 activate 新 epoch | `store/events/mod.rs:704-708` 可能 fence 仍在 append 的旧 writer。这是运维窗口问题而非 bug，但需要写进 runbook |

### 暂缓项及理由

| 项 | 理由 |
|---|---|
| 拆分 crate（`pchronicle-core` / `pchronicle-store`） | 概念上正确——Gateway 只需要 model + document codec，不需要 Lance——但属于 breaking change，且当前 workspace 内消费者已通过 `default-features = false` 拿到了大部分收益。建议先做 A-7 把门面立场敲定，再评估是否值得拆 |
| `operations/` 从核心 crate 迁出 | 全部 `#[cfg(feature = "search")]`，与写入 / 投影管线无关。因 search 按项目约定排除在本次范围外，只记录位置不提动作 |
| `interop.rs` 的 HAR / OTLP 导出风格统一 | stringly-typed 分支与 typed LLM payload 风格不一致，但该模块是外围导出，改动收益低于 P2 各项 |

### 与当前工作区未提交改动的关系

评审时 `git status` 显示 15 个新的 `tests/proptests/*.rs` 正在加入（`actf`、`atif`、`coords`、`detect`、`events`、`input`、`json_stream`、`jsonl`、`llm`、`registry`、`revision`、`timestamp`、`format`、`agenticmd_codec`、`agenticmd_layout`），对应 `tests/README.md:17-25` 记录的三批迁移。方向正确。

按本次评审的风险排序，**第四批最该覆盖的是 `unknown_fields`**——它是整个 crate 最复杂的不变量集合（pointer 转义、envelope 版本、carrier 绑定、冲突 fail-closed、限额），目前只有单元测试没有生成式测试。其次是 `formats/storyline` 的 validate / serialize 组合，以及 `layout/resolve` 的路径推断（见 A-14）。

## 核实清单

以下断言在评审中逐条读过原文，不依赖二手结论：

| 断言 | 核实位置 |
|---|---|
| `analysis_compile` 是公开模块，search 下有 crate 根 re-export | `lib.rs:31`、`:71-81` |
| `DocumentFormat` 有 9 个变体 | `format.rs:11-30`、`:33-43` |
| `in_flight` 只覆盖 `try_send` 窗口；`finish()` 忙等 | `append_queue.rs:132-151`、`:166-168` |
| events provider 对任意 filter 返回 `Exact` | `store/events/datafusion.rs:120-128` |
| `DocumentSource` capabilities 中 CanonicalEvent 为 `FilterPushdown::Exact` | `store/document_source.rs:102-110` |
| `query()` 全量 collect；bounded 路径存在且不回滚已写批次 | `store/query_engine.rs:347-354`、`:401-439` |
| `expand_story_locations_blocking` 每次新建 runtime | `discovery.rs:95-100` |
| README 与实现的四模块 / 六格式 / 能力位差异 | `README.md:26-35`、`:64-66`、`:80-87`、`:95-106` |
