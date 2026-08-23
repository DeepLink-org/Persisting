# pChronicle 设计与实现深度评审

> 视角：分布式系统与存储。重点：过度设计识别与"刀法"建议。
> 范围：`crates/persisting-pchronicle`（约 109 个 Rust 文件）+ `crates/persisting-pchronicle-cli`（约 7.3 万行合计，含测试）。
> 日期：2026-08-23

---

## 0. 一句话总评

pChronicle 的**核心数据模型和写入路径是健康的**（append-only events → 单向投影 → Lance 三表），但它在三个维度上系统性超配：**用多写者分布式协议保护一个单机嵌入存储、用 9 种格式服务一个 1:1 的 schema、用手写查询优化器绕过已有的成熟优化器**。估算总代码中约 1.2–1.5 万行（近 20%）服务于边际收益极低的需求，且这些复杂度集中在最容易腐烂的并发协议与格式转换上。

## 1. 架构骨架（先说清楚它是什么）

```
capture 回调
  → append_queue（有界 mpsc + 专用线程 + 2-worker tokio runtime）
  → events.lance（per-run，append-only，事实层）
  → projection（单向投影为 StorylineDocument）
  → storyline store（runs/steps/tool_calls 三表 + objects.lance 内容寻址）
  → CURRENT 指针 CAS 发布
读取：DataFusion (ChronicleQueryEngine) → DocumentSourceImpl → 4 种 datasource
CLI/server：import/export、query、explorer HTTP API、acceleration 内存索引、analysis compile
```

亮点（必须承认做得好的部分）：

- ** Lance 单引擎决策正确**。没有自研存储格式，MVCC/索引/compaction 全部复用 Lance，Arrow/DataFusion 打通内存与查询。这是整个项目最值钱的一个取舍。
- **trait 使用克制**。crate 自有 trait 仅 2 个（`ChronicleEventRecordExt`，`formats/events.rs:48`；`QueryDocumentSource`，`document_source.rs:28`，pub(crate)），没有 trait 爆炸。注：另有 5 处 `impl TableProvider`（storyline/datafusion、catalog/provider、files、events/datafusion、agenticmd_datafusion），那是对 DataFusion 外部 trait 的实现而非自有抽象——但它们正是 §2.6 合并刀口的对象，两处结论自洽。
- **写入路径的 epoch fence 思路正确**：旧 epoch 只能产生垃圾、不能产生可见数据。
- **测试文化健康**：CLI 层测试与实现约 2:1，且以端到端行为测试为主（import 原子性、预算截断、错误策略），不是镜像内部结构的脆测试。

问题不在骨架，在骨头上长的赘肉。以下按"砍掉的收益/风险比"排序。

---

## 2. 过度设计清单（按动刀优先级排序）

### 2.1 多写者分布式 lease 协议保护单机存储 —— 最大的刀口

**现状与证据**：

| 组件 | 行数 | 职责 |
|---|---|---|
| `store/writer_control.rs` | 628 | 完整 writer lease：acquire/renewal/takeover/CAS publish |
| `store/events/manifest.rs` | 1006 | epoch fence + 分层 segment 压缩 + 64 次 CAS 重试 |
| `store/run_control.rs` | 858 | per-run lease/commit CAS |
| 其他 | — | root_write_lock、dataset_write_lock、index_build_gate、append_queue 双检 |

加起来 **2500+ 行并发协议代码**，外加 4 组 `#[cfg(test)]` 全局 barrier/故障注入 hook——分布在 `store/storyline/mod.rs:292-317`（`CREATE_AFTER_EMPTY_READ_BARRIER`、`REPLACEMENT_AFTER_CURRENT_READ_BARRIER`）和 `projection/storyline.rs:28-32`（`BUILD_BEFORE_PUBLICATION_BARRIER`、`PROJECT_SOURCE_READ_FAILURE`）——**并发协议复杂到必须注入故障注入点才能测试，这本身就是复杂度失控的信号**。

**为什么过了**：对象存储后端（s3://az://gs://）是这套协议唯一真正的存在理由，而它只是 `open_uri` 注释里预留的插件点（`store/mod.rs:435-439`）。单机嵌入场景下，代码里其实**已经存在正解**：`acquire_write_guard` 的 flock（`store/mod.rs:504-539`）+ `write_local_current` 的原子 rename（L1535）。lease/takeover/CAS 全部是在为一个尚未接入的后端买单。

**刀法**：
- 现在：本地路径走 flock + rename，writer_control 整体降级为一个 feature-gated 模块或直接删除，对象存储支持声明为"roadmap"。省约 1000–1500 行，并消掉全部 takeover/cleanup 分支和故障注入 hook。
- 真到接 S3 那天：用 Lance 自身的外部 manifest store（它本来就支持 commit 抽象），不要自己写 CAS 协议。
- **原则**：分布式协议的复杂度不能"预埋"。预埋的协议代码不是资产，是每轮重构都要搬运的负债。

### 2.2 9 种数据表示、5 种轨迹表示 —— schema 是 1:1 的，表示却是 1:5 的

**口径说明**：`DocumentFormat` 枚举为 7 个变体（`format.rs:11-26`：CanonicalEvent / Storyline / StorylineLance / AgenticMd / Atif / OpenaiMsg / Actf）；"9 种"的完整口径 = 7 个登记变体 + 2 个未登记表示（`formats/llm.rs` 的 LLM payload、`convert/actf.rs` 内藏的 OpenClaw 事件日志子格式）。另有 3 种控制面 JSON（manifest / CURRENT / run-control）未计入。

**证据**（`format.rs:11-26` + 逐字段对比）：

- **Storyline JSON ≈ ATIF，几乎是字段改名**。`AtifStep{step_id, timestamp, source, message, reasoning_content, ...}` 对照 `StorylineTurn{id, ts, src, msg, reason, ...}`（`formats/storyline.rs:121-161`），`lib.rs:5` 自认"与 ATIF v1.7 对齐"。`convert/atif.rs` 943 行大部分在做 rename + 短键映射。
- **ACTF 概念泄漏进 hub**：`StorylineTaskResult`（storyline.rs:364-393）的 `correct/final_answer/ground_truth/score/...` 14 个字段是纯 benchmark 打分语义，hub 被 ACTF 污染。
- **ACTF 内部还藏着第三种格式**：untagged `ActfTrajectoryWire::Events` + `convert/actf.rs:297-587` 的 openclaw_* 函数族，一个未被 `DocumentFormat` 承认的 event-log 格式。
- **边界混乱**：openai_msg 的双向转换不在 `convert/` 而在 `formats/openai_corpus.rs`（含约 450 行反向 synthesize）；`pointer_join` 出现 3 次、`message_text` 2 次、`insert_*_map` 3 份。
- **文档与实现矛盾**：lib.rs 声称 events→Storyline 是单向投影，但 `storyline_to_events` 存在并被 `store/catalog/mod.rs:413,430` 用于重建 events。

**刀法**：交换格式 5 砍到 2——保留 **ATIF**（外部标准，Harbor RFC 0001）和 **AgenticMD**（人类可读）。Storyline JSON 退化为内部类型别名（不再作为交换格式暴露），ACTF/OpenAI Msg 语料移出核心、降级为独立 import 工具或声明有损导入。省约 3000 行，且 hub schema 从此只需要对齐一个外部标准。

### 2.3 unknown_fields 机制：2500+ 行为一个弱需求

**证据**：`formats/unknown_fields.rs` 1318 行，实现"把源格式中 hub 不认识的字段按 `(source_format, document_id, JSON pointer)` 三元组保留、导出时写回原位置"，外加每个转换器里的寄生 capture/restore 逻辑（actf.rs:664-836 约 170 行、openai_corpus 约 250 行、agenticmd restore），总成本 **2500+ 行**。

**为什么过了**：同 crate 里 `atif.rs:30-31` 已经用 `#[serde(flatten)] unknown: Map<String, Value>` 廉价解决了同一问题；而且 `storyline.rs:19` 全部结构体 `deny_unknown_fields`——**权威模型本身根本不需要这套保留机制**。为一个"roundtrip 不丢陌生字段"的弱需求付出一套分布式追踪级别的基础设施。

**刀法**：全面改用 `#[serde(flatten)]` 的 per-struct unknown map（同 format 内 roundtrip 即可），跨格式转换时仅告警不保留。省 2000+ 行，语义对 99% 用户无差别。

### 2.4 acceleration.rs：手写了一个 miniature 查询优化器

**证据**：`crates/persisting-pchronicle-cli/src/server/acceleration.rs` 1762 行（**CLI crate 的 server 子模块，非核心存储 crate**）——用 sqlparser 解析用户 SQL（`AnalyzedQuery`），保守注入 `_file_ IN (...)` 谓词做 source 裁剪；自建三套内存索引（run 摘要缓存、run 路由索引、上限各 100 万行的 value→source 指纹索引）。其注释声明 persistent Catalog 仍是 source of truth、裁剪失败时回退原查询，即它是一个保守可降级的加速层——位置与可降级性使其影响面小于核心查询引擎内嵌优化器，但不改变下面的结论。

**为什么过了**：DataFusion 自带谓词下推与分区裁剪，Lance 自带标量索引。手写 SQL 重写器是在和一个成熟优化器赛跑，而且只能"保守地"跑——典型的负和博弈：写 1762 行，换来的是 DataFusion 升级时必须跟着维护的 AST 分析代码。

**刀法**：`_file_` 裁剪下推给 DataFusion/Lance；value→source 映射持久化为一张小物化表，用普通 SQL join 实现；run 摘要缓存退化为简单的 TTL cache。省约 1400 行。

### 2.5 "分析"功能的三份实现

同一个"按 agents/models/tools 维度聚合"的需求（证据链完整，均可直接跳转）：

1. CLI 的 4 条硬编码 SQL 常量：`pchronicle-cli/src/lib.rs:1944-2028`（`ANALYSIS_OVERVIEW_SQL` / `ANALYSIS_AGENTS_SQL` / `ANALYSIS_MODELS_SQL` / `ANALYSIS_TOOLS_SQL`，由 L1852-1855 的 `AnalysisCommand` 分派消费）
2. `pchronicle-cli/src/server/explorer.rs` 的 Rust 侧聚合：`analyze`（L440）+ `dimension_aggregates`（L782），直方图/百分位/维度聚合
3. spec→SQL 编译管线：`analysis_compile.rs`（核心 crate）+ 前端 `analysis.rs`(2538 行) + `analysis_session.rs`(2332 行) + `analysis_agent.rs`(1528 行)

**刀法**：统一到 analysis compile 管线（它已有 stale_snapshot/EXPLAIN 校验，最严谨），CLI `analysis` 子命令改为调 compile，删硬编码 SQL 与 explorer 的 Rust 聚合。顺带把 `analysis_compile.rs`（1068 行）从存储 crate 移到 CLI crate——它唯一的调用方就在 CLI，存储 crate 不该为 LLM 分析功能背书。

### 2.6 catalog 多 mount 查询层

**证据**：`store/catalog/`（6 文件约 3000 行）实现多 mount union + LazySource + 第五套 DataFusion TableProvider + `_file_` 谓词下推。

**刀法**：要求数据先投影进单一 Storyline store，用 DataFusion 原生 `UNION ALL` 视图替代 LazySource/provider 体系。五套 TableProvider（storyline/datafusion、events/datafusion、files、agenticmd_datafusion、catalog/provider）合并为至多两套（events + storyline），样板代码随之消失。

### 2.7 功能面赘肉（单项不大，合计可观）

- **`onboard.rs`（1087 行 + 3 份内嵌资产）**：教程不该是可执行代码，还要进程内回调 `run_list/run_query` 捕获输出再渲染。换成静态 Markdown 文档 + `pchronicle demo` 生成示例数据集。
- **`echo` 子命令**：测试工具混进产品 CLI，移到 gateway crate 的 dev-bin。
- **`/export/har`、`/export/otlp`、`/revisions` 三个单用途端点**：合并为 `/api/export?format=har|otlp`；revisions 前端未调用（`api.rs` 无引用），删。
- **`/api` 与 `/api/v1` 双前缀**（`server/mod.rs:184-211` 等价路由注册两遍）：留其一。
- **`QueryDocumentSource` trait**（`document_source.rs:28`）：唯一实现者就是同文件的 `DocumentSourceImpl` enum，方法全部委托回 enum match——trait+enum 双重抽象，删 trait 无损失。
- **四层 re-export 门面**（lib.rs → storage.rs → store/mod.rs → storyline/mod.rs）：同一符号转出口 4 次，收敛到 2 层。
- **agenticmd 写路径**：自称"非权威 debug 视图"却有 `fs.rs`（814 行）的 upsert/索引/元数据重写 + 专属 TableProvider。debug 视图不该有写路径，砍 `fs.rs` 只留渲染。
- **objects.lance 内容寻址**（`content.rs` 1121 行的 externalize/hydrate/prune + GC）：Lance blob 列或提高阈值直存大 JSON 即可覆盖大多数场景，`prune_unreferenced_objects` 随之消失。这条优先级最低，因为它确实解决大 payload 问题，但值得重新标定阈值验证收益。

### 2.8 serve 三合一编排

`serve` 把 Warehouse HTTP + Control JSONL/TCP 写协议 + LLM Gateway 编排进一个进程，配套 110 行 shutdown 状态机 + `projection_supervisor` 539 行 per-source 重试状态机。单一职责拆分：Gateway/Control 独立子命令，Warehouse 只做读；投影收敛用一次性 `converge_before_readiness` + 后台定时任务即可。

---

## 3. 刀法汇总：砍/留/改

| 优先级 | 动作 | 目标 | 预估省代码 | 风险 |
|---|---|---|---|---|
| P0 | 砍 | writer_control lease + 对象存储预留（改 flock+rename） | ~1500 行 | 低，未来接 S3 用 Lance commit 抽象 |
| P0 | 砍 | unknown_fields 机制（改 serde flatten） | ~2000 行 | 低，跨格式 roundtrip 变有损（可接受） |
| P0 | 砍 | acceleration.rs（下推给 DataFusion + 物化小表） | ~1400 行 | 中，需验证查询延迟回归 |
| P1 | 合并 | 交换格式 5→2（ATIF + AgenticMD） | ~3000 行 | 中，外部用户迁移成本 |
| P1 | 合并 | analysis 三份实现 → compile 管线 | ~1500 行 | 低 |
| P1 | 合并 | catalog 层 → DataFusion 原生 union；TableProvider 5→2 | ~2000 行 | 中 |
| P2 | 移动 | analysis_compile.rs → CLI crate；agenticmd 砍写路径；onboard 改静态文档 | ~2000 行移出核心 | 低 |
| P2 | 清理 | 双 API 前缀、QueryDocumentSource trait、4 层 re-export、echo/revisions | ~500 行 | 低 |

**合计：约 1.2–1.4 万行从核心链路移除（当前总量 7.3 万行含测试），核心 crate 预计瘦身 25–30%，且砍掉的全是故障率最高的并发协议与格式转换代码。**

## 4. 边际收益曲线视角的总结

这个项目的复杂度分布呈现一个清晰的模式：**每一项过度设计都对应一个"未来可能要"的需求**——对象存储、多写者、五种交换格式、无损 roundtrip、手写优化器——而这些需求至今没有一个真实落地。与之相对，真正落地的需求（单机嵌入、ATIF 交换、DataFusion 查询）都已经有更简单的现成解。

刀法精准的判据只有一条：**为已验证的需求付复杂度，不为想象中的需求付复杂度；当简单方案（flock、serde flatten、DataFusion 下推）已经存在于代码库自身时，它就是正确答案的证据，而不是权宜之计。**

好消息是骨架不用动：events 事实层 + 单向投影 + Lance 三表这个核心是对的。所有的刀都落在附加层上，这也是为什么上面的每项砍除风险都只有"低"或"中"——它们本来就不该在关键路径上。

---

## 5. 第二轮意见的吸收（2026-08-23 补充）

另一份评审对本文档提出了三点修正和若干新发现。经逐条代码核实，**其新发现全部属实，其修正我接受两条半**。核实证据与合并后的结论如下。

### 5.1 核实通过的新发现

**(a) 三套逐行同构的 CAS store —— 确认，且比本文档 §2.1 的定性更精确。**

`store/attempt_registry.rs`（409 行，第一轮漏看）、`store/run_control.rs`、`store/events/manifest.rs` 三段代码结构完全一致，证据链：

| 元素 | attempt_registry.rs | run_control.rs | manifest.rs |
|---|---|---|---|
| `CAS_RETRIES` | L20 (=32) | L22 (=32) | L24 (=64) |
| `enum Backend {Local, Object}` | L48 | L43 | 隐含于 mutate_with_mode |
| `async fn mutate<T, F>` 闭包模式 | L216 | L368 | L498 (`mutate_with_mode`) |
| `PutMode::Create` / `Update(version)` | L257-258 | L412-413 | L554-556 |
| `read_local_*` / `write_local_*`（tmp+rename） | L315/328 | L478/491 | L603/615 |
| `encoded_id` 路径编码 | L294 | 同款 | — |

第二份意见的定性是对的：**这不是"三个系统重复解决一个问题"，而是"一个正确性论证（读-改-CAS 写）被抄了三遍"**。问题在抽象缺失，不在职责重复。这把 §2.1 的行动建议从"砍 lease"修正为"先抽象"——见 §5.3。

**(b) `SingleWriter` 双写模式 —— 确认。** `manifest.rs:27-33` 定义 `Conditional`/`SingleWriter` 双轨，为"不支持条件替换的 S3 提供商"准备，在 RFC-0007 已定调本地 loopback sidecar 的当前阶段无真实使用者。属提前工程，归入 §2.1 同一刀口。

**(c) ATIF 的 unknown 被处理两遍 —— 确认，比我第一轮说的更严重。** `src/atif.rs` 的 5 个结构体全部自带 `#[serde(flatten)] unknown: Map<String, Value>`（L30/44/74/87/94），而 `convert/atif.rs:287` 又手写 `capture_atif_unknowns` 用 JSON pointer 再捕获一遍。声明式 flatten 与手写指针捕获语义重叠、各自维护。这强化了 §2.3 的结论：unknown_fields 机制不是"可以更便宜"，而是"在同一格式内部就已经自相重复"。

**(d) 两个空 `mapping/` 死目录 —— 确认。** `src/mapping/` 与 `src/agenticmd/mapping/` 均为空（2026-08-17 重构残留），直接删。

### 5.2 接受的修正

| 第一轮判断 | 修正后 |
|---|---|
| §2.1："砍 writer_control lease 体系，省约 1000 行" | **降级为两步**：先抽 `cas_store` 原语消除三份同构（约 600 行重复下沉），对象存储后端是否删除另作独立决策。直接删 lease 会把"防本地多进程"的真实保护一起删掉，风险被低估 |
| §2.7："objects.lance 内容寻址值得重新标定阈值验证收益" | **撤回，改为明确保留**。它是作用于三表全部宽列的通用 content-addressed 大对象层，解决 `reasoning_content`/`message_json` 的行宽与重复痛点，是刀刃上的钢。第二轮意见对"它是 unknown-fields 去重附属品"的反驳成立 |
| epoch fence 语义 | 两论一致：**保留**。这是写入路径正确性的核心 |

### 5.3 合并后的刀法清单 v2

| 优先级 | 动作 | 省代码 | 性质 |
|---|---|---|---|
| **P0** | 抽 `cas_store` 原语（read-if-match + 重试，本地锁/对象 store 双后端），三个 store 下沉复用 | ~600 行 | 消除冗余（两份意见汇合后的第一刀） |
| **P0** | 收敛格式：`atif.rs` 的 flatten `unknown` 与 `capture_atif_unknowns` 合并为一条路径；unknown_fields 全局机制改 serde flatten | ~2000 行 | 冗余 |
| **P0** | 砍 acceleration.rs（下推 DataFusion + 物化小表） | ~1400 行 | 过度设计（仅第一轮提出，第二轮未反对） |
| **P1** | 格式收敛：2 个存储权威（Events Lance + Storyline Lance）+ 其余降级为纯 import codec，砍掉 5 个 codec 的 DataFusion provider 与 7 分支能力矩阵（`document_source.rs:260-326`） | ~3000 行 | 冗余+过度设计（两论方向一致，第二轮的"砍 query provider 但保留 import codec"比第一轮的"砍格式"更稳妥，采纳） |
| **P1** | 删 `SingleWriter` 双写模式；catalog 层 → DataFusion 原生 union | ~2500 行 | 过度设计 |
| **P2** | analysis 三份实现合一；analysis_compile.rs 移到 CLI；agenticmd 砍写路径；onboard 改静态文档 | ~2000 行移出核心 | 第一轮提出，第二轮未涉及，保留 |
| **P2** | 删空 `mapping/` 目录 ×2；评估 Storyline writer lease 续约退化为单写者断言 | ~100 行 | 冗余/过度设计 |

**保留清单（两论一致）**：Lance 单引擎、events→Storyline 单向投影、epoch fence、`content.rs` content-addressed 层、CLI 行为测试套件。

### 5.4 两轮意见的关系

第二轮的价值不在于推翻第一轮，而在于**把"过度设计"拆成了两个性质不同的桶**：

- **冗余（复制粘贴）**：三套 CAS、两套 unknown 捕获、空目录——这类问题解法是抽象，风险极低，应最先动；
- **提前工程（为未出现场景的复杂度）**：SingleWriter、lease 续约、多跳 envelope、acceleration、多 codec provider——这类问题解法是做减法和降级，需要逐项验证无真实使用者。

第一轮按"砍多少行"排序，第二轮按"问题性质"分类。合并后的正确顺序是：**先做零风险的抽象（CAS 原语、unknown 单路径），再做需要验证的减法（provider、SingleWriter、lease 降级）**——这正是边际收益曲线上"先摘低垂果实"的标准打法。

---

## 6. 第三轮交叉核验的裁决（2026-08-23 补充）

第三轮核验者对本报告 §0-§4 提出 2 处事实性质疑和 3 处口径质疑。经逐条回到代码钉死，**裁决如下：本文档 2 处措辞不精确（已修正），1 处双方各对一半，3 个数字口径全部补证成立**。

### 6.1 数字口径的补证（第三轮的三个"待补证"全部落实）

| 被质疑论断 | 裁决 | 证据 |
|---|---|---|
| "analysis 三份实现"证据链断裂 | **成立，证据链已补齐**（§2.5 已更新行号）。第三轮只找到 1 份是因为漏了 CLI 的 4 条硬编码 SQL 常量（`pchronicle-cli/src/lib.rs:1944-2028`，由 L1852-1855 的 `AnalysisCommand` 分派消费）和 `explorer.rs` 的 `analyze`(L440)+`dimension_aggregates`(L782)。三份=CLI 硬编码 SQL + explorer Rust 聚合 + compile 管线，置信度升为**高** | 见 §2.5 |
| "9 种格式"口径不明 | **口径钉死**：7 个 `DocumentFormat` 登记变体（`format.rs:11-26`，第三轮实测一致）+ 2 个未登记表示（`formats/llm.rs` LLM payload、ACTF 内藏 OpenClaw 子格式）= 9。§2.2 已补充口径说明 | `format.rs:11-26` |
| "全 crate 仅 2 个 trait"会被抓漏洞 | **第三轮的提醒成立，已修正措辞**：自有 trait 2 个（`ChronicleEventRecordExt` pub、`QueryDocumentSource` pub(crate)），另有 5 处对 DataFusion 外部 trait `TableProvider` 的实现。§1 亮点已改为带口径的表述 | `formats/events.rs:48`、`document_source.rs:28`、5 处 impl |

### 6.2 事实性裁决

**(a) 故障注入 hook 的位置——双方各对一半，真相是更多。**

- 本文档第一版写"`store/mod.rs:301-406`"，实际路径是 `store/storyline/mod.rs:292-317`（路径截断笔误，已修正）；
- 第三轮写"hook 在 `projection/storyline.rs:28-34`，`writer_control.rs` 里没有"——`projection/storyline.rs` 的 hook 属实，但第三轮漏掉了 `store/storyline/mod.rs` 里的另外两组（`CREATE_AFTER_EMPTY_READ_BARRIER` L302、`REPLACEMENT_AFTER_CURRENT_READ_BARRIER` L313）；
- **事实全貌：4 组全局 barrier/故障注入 hook，分布在 2 个文件，都不在 `writer_control.rs`**。本文档从未把 hook 归于 writer_control（第三轮引用的"安到 writer_control"是对本文档 §2.1 的误读），但路径确实写错过。实质观察（并发协议需要故障注入才能测试）经三轮核实**反而被加强**——hook 比任何一方说的都多。

**(b) acceleration.rs 的位置与定性。**

本文档自始至终引用的是 `server/acceleration.rs`（即 CLI crate 的 server 子模块），第三轮先误判本文档"说它在核心 crate"，随后自行纠正并确认位置——**此项无分歧**。接受第三轮的两点改进并已并入 §2.4：标注完整 crate 路径；补充"保守可降级加速层、Catalog 仍是 source of truth"的定性。该定性微调**不改变结论**：1762 行的 sqlparser 重写器 + 3 套内存索引对"物化小表让 DataFusion 自己剪"仍是负和账，但它位于 CLI 层、可整体丢弃、不污染核心，因此 v2 清单中其优先级维持 P0（收益/风险比高）而非升级为"核心引擎问题"。

### 6.3 三轮汇合后的最终刀法（置信度标注版）

| 优先级 | 动作 | 置信度 | 汇合情况 |
|---|---|---|---|
| P0 | 抽 `cas_store` 原语，三 store 下沉复用 | 高 | 三方独立证实同构 |
| P0 | unknown_fields → serde flatten 单路径 | 高 | 三方独立证实 flatten 已存在 |
| P0 | acceleration → DataFusion 下推 + 物化小表（CLI 层） | 高 | 第一轮提出，第三轮确认位置与定性 |
| P1 | 交换格式收敛：2 存储权威 + 其余降级纯 import codec | 高 | 三轮方向一致 |
| P1 | TableProvider 5→2（catalog → DataFusion 原生 union） | 高 | 第一轮提出，第三轮验证 5 处 impl 存在 |
| P1 | analysis 三份实现 → 统一 compile 管线 | **高**（证据链已补齐） | 第一轮提出，第三轮质疑后补证成立 |
| P1 | 删 SingleWriter 双写模式 | 高 | 第二轮发现，未受质疑 |
| P2 | ACTF benchmark 语义剥离出 hub | 中 | 三方证实泄漏，剥离方案待设计 |
| P2 | 空 mapping 目录、双 API 前缀、onboard 静态化等 | 高 | 低垂果实 |

**三轮核验的元结论**：本报告的所有 P0/P1 刀口均已被至少两方独立核实；第三轮的全部质疑已闭环——要么是口径问题（已钉死），要么是双方各对一半（hook 位置，真相更强）。没有任何一刀因交叉核验而被撤销，反而有两刀（analysis 三份、hook 信号）因质疑而被加强。可以进入重构提案阶段。

---

## 7. 施工清单 Review（§6.3 的可执行性审查）

以 §6.3 为施工蓝本逐条审查。**总评：刀口选择正确、置信度可信，但作为施工列表缺三样东西——依赖顺序、验收标准、以及 2 个在前几轮丢失/倒挂的条目。** 直接照单施工会在两处卡住。

### 7.1 逐条裁决

| 条目 | 施工裁决 | 问题 |
|---|---|---|
| P0 cas_store 原语 | ✅ 可施工，**但需前置一个小 PR** | ① SingleWriter 删除（P1）应**前移进本刀**：否则原语必须参数化一个即将死掉的模式，提取面凭空变大。② `CAS_RETRIES` 三个 store 为 32/32/64，统一值需显式决策（manifest 的 64 是有意的还是随手写的，无从考证——建议统一为 64 并留注释）。③ 抓手确认：三个 store 各有独立测试 mod（attempt_registry:376、run_control:540、manifest 内），行为保持型重构可验收 |
| P0 unknown → flatten | ⚠️ **顺序有坑** | 与 P1 格式收敛存在依赖倒挂：若先做 flatten，要改写 `convert/actf.rs` 等约 170 行×若干的捕获逻辑——而这些文件在格式收敛后**整个被删**，纯返工。正确顺序：先做格式收敛的**范围决策**（哪些 codec 幸存），再对幸存者做 flatten |
| P0 acceleration | ✅ 可施工，**但验收标准缺失** | 已核实前端 5 个文件（`llm_settings.rs`/`analysis.rs`/`result_explorer.rs`/`analysis_agent.rs`/`components.rs`）真实消费 evidence 接口——这不是无人使用的死代码。拆除必须有**延迟回归门槛**（fixture 数据集 p50/p95 前后对比），否则是盲拆。与 P1 catalog 合并有隐含依赖：acceleration 注入的 `_file_` 谓词由 catalog provider 下推消费，先砍 acceleration、后并 catalog 的顺序是对的，但清单未写明 |
| P1 格式收敛 + P1 TableProvider 5→2 | ⚠️ **应合并为一个 epic** | 5 个 codec provider 的删除本来就是格式收敛的组成部分，拆成两条独立 P1 会造成大量中间态（codec 还在、provider 已删，或反之）。另缺**外部兼容策略**：`DocumentFormat` 是公开枚举、import/export 是 CLI 公开接口，需要一个 deprecation/alias 计划，否则是 breaking change |
| P1 analysis 三合一 | ✅ 可施工，**与 P2 有一条顺序倒挂** | 应先做纯机械的 `analysis_compile.rs` 移 crate（P2、零行为变化），再做三合一（行为变化）。否则统一工作要在错误的位置做一遍再搬家。验收标准建议：CLI `analysis` 子命令对 fixture 数据集做 **golden diff**（统一前后 SQL 结果等价） |
| P2 各项 | ✅ 无异议 | 空目录、双前缀这类可随手热身 |

### 7.2 清单缺漏（三轮评审中有、施工清单中丢了的）

1. **多跳 `_storyline` envelope 删除**——第二轮 P1 明确提出（"unknown 无损降为单跳"），§6.3 里丢失了。应并入格式收敛 epic。
2. **`openai_corpus.rs` 的转换逻辑移入 `convert/`**——第一、二轮都点名的边界统一，属格式收敛 epic 的子任务。
3. **横切验收基建**：整个清单没有一条性能回归门。建议先建一个最小 benchmark fixture（现成的 benchmark/ 目录数据即可）+ p50/p95 报告脚本，作为 P0-3 和 P1 catalog 两刀的共享门槛。
4. **构建耦合提示**：CLI 通过 build.rs `include_dir!` 嵌入前端 WASM，所有动 CLI server 层的 PR 构建失败会连带前端——施工顺序上应先解耦（asset 改运行时加载或 feature-gate），否则每个 server 层 PR 都背负双端构建。

### 7.3 修正后的 PR 序列

```
PR0   热身：删空 mapping/ 目录 ×2、/api 双前缀留一、echo→dev-bin、revisions 删
      （~150 行，零风险，验证 CI 通路）
PR1   删 SingleWriter 双写模式（独立小 PR，缩小 cas_store 提取面）
PR2   cas_store 原语 + 三 store 迁移（1 个新模块 PR + 3 个迁移 PR）
      验收：现有测试全绿 + 原语单测（并发互斥/CAS 重试/本地锁）+ 三 store 测试不动
PR3   analysis_compile.rs 移到 CLI crate（纯移动，零行为变化）
PR4   格式收敛 RFC（决定 2 权威 + import codec 幸存清单 + deprecation 计划）
      → 执行 PR：codec 降级 + provider 5→2 + unknown flatten（对幸存者）+ envelope 删除
        + openai_corpus 移 convert/
PR5   性能门基建（benchmark fixture + p50/p95 报告）     ← 可与 PR1-4 并行
PR6   acceleration 拆除（验收：PR5 门槛内 + 前端 evidence 功能回归）
PR7   analysis 三合一（验收：golden diff）
PR8   catalog → DataFusion 原生 union（依赖 PR6：_file_ 裁剪的归宿先定）
PR9   P2 余项：agenticmd 砍写路径、onboard 静态化、lease 续约降级评估
```

依赖关系：PR1→PR2→(PR4 可并行)；PR5→PR6→PR8；PR3→PR7。关键路径 = PR0→PR1→PR2→PR4，约占总收益的一半以上。

### 7.4 范围合规

全部条目落在 `persisting-pchronicle` / `persisting-pchronicle-cli` 两个 crate 及 `pchronicle-web` 构建配置内，符合 AGENTS.md 默认 scope（不触碰 queue/search/TTAS/dlcapt）。验收命令建议用 `-p persisting-pchronicle -p persisting-pchronicle-cli` 定向跑，避免拉入排除子系统。
