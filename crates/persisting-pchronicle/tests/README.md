# pChronicle 测试布局

pChronicle 的测试分四层，放置位置与运行方式如下。

| 层级 | 位置 | 覆盖对象 | 运行方式 |
|---|---|---|---|
| 单元测试 | `src/**/tests.rs` 或小模块内 `#[cfg(test)] mod tests` | 与被测模块同目录：单模块/单函数行为、格式解析、存储协议、并发语义 | `cargo test -p persisting-pchronicle --lib` |
| 性质测试 | 私有模块放在对应 `src/**/*.rs` 的 `#[cfg(all(test, feature = "proptest"))] mod proptests`；公开接口放在 `tests/proptests/*.rs` | 跨输入空间的编码往返、验证不变量、路径与错误语义 | 日常功能测试不编译；回归用 `just proptest pchronicle` |
| 集成测试 | `tests/*.rs` | 跨模块/跨 crate 行为：存储往返、查询引擎、格式 corpus、真实 S3 契约 | `cargo test -p persisting-pchronicle` |
| 基准 | `benches/*`、`benchmark/pchronicle/` | 转换与存储性能回归 | `just benchmark-pchronicle` |

`tests/proptests/` 中的每个公开接口性质测试都是独立的 Cargo test target，不再通过 `include!` 转接。所有性质测试 target 和源码内私有 `mod proptests` 都由 `proptest` feature 控制：`just test pchronicle` 只编译和运行功能测试，pChronicle 完整回归执行 `just proptest pchronicle`。

大型存储模块使用邻接测试文件：`store/events/tests.rs` 覆盖 fencing/append，
`store/storyline/tests.rs` 覆盖 CURRENT 原子性，`store/files/tests.rs` 覆盖 `_file_` 裁剪；
较小模块（如 `store/agenticmd_fs.rs`）仍可保留内联 `mod tests`。
crate 级门面行为（格式往返、detect、frontmatter 解析）集中在 `src/tests.rs`。

性质测试按风险优先级分批迁移；当前首批 5 个模块为：
`format`（格式识别入口）、`input`（错误语义）、`atif`（轨迹校验）、
`revision`（派生版本落盘编码）和 `formats/events`（Canonical Event 信封）。
第二批 5 个模块为：`formats/llm`（LLM payload 与 stream 事件）、
`formats/actf`（ACTF 轨迹校验）、`formats/timestamp`（纳秒精度）、
`formats/common/json_stream`（流式 JSON 边界）和 `formats/common/jsonl`（JSONL 行语义）。
第三批 5 个模块为：`agenticmd/codec`（Markdown block 编解码）、
`agenticmd/layout`（会话文件路径安全）、`formats/detect`（格式指纹优先级）、
`formats/registry`（codec 注册一致性）和 `layout/coords`（运行分区坐标）。
第四批 5 个模块为：`interop`（HAR/OTLP 互操作）、`document`（文档 codec 门面）、
`agenticmd/convert`（Storyline 与 Markdown 语义往返）、`formats/storyline`
（权威模型与严格 wire）和 `formats/unknown_fields`（JSON Pointer 与未知字段保真）。
第五批 5 个模块为：`formats/codex`（Codex JSONL 会话解码）、
`formats/claude_code`（Claude Code transcript 解码）、`formats/atif`
（ATIF 与 Storyline 转换）、`formats/actf/convert`（ACTF 语义转换）以及
`store/storyline/model`（Storyline 三表拆分与重建）。
第六批 5 个模块为：`formats/codec`（codec 通用候选路径与批量发射）、
`agenticmd/validate`（AgenticMD speaker/type/pointer 安全校验）、
`formats/events/convert`（Canonical Event 与 Storyline 语义转换）、
`layout/resolve`（轨迹路径推断与显式参数合并）以及
`store/location`（本地/对象存储 URI 规范化与 bucket 校验）。
第七批 5 个模块为：`agenticmd/fs`（AgenticMD 文件块编码与 span）、
`formats/openai_corpus`（OpenAI corpus ID 与相对路径安全）、
`store/storyline/content`（内容 descriptor 与 UTF-8 preview）、
`store/storyline/rows`（Storyline steps Arrow 行编解码）以及
`projection/storyline`（Canonical projection lineage 新鲜度）。
第八批 5 个模块为：`store/events/manifest`（事件 manifest 与 writer fence）、
`store/storyline/writer_control`（Storyline CURRENT lease 状态转换）、
`store/run_control`（Run control revision 与冲突边界）、
`store/catalog/identity`（namespace 与 SQL alias 身份规范）以及
`store/catalog/namespace`（快照分页 token 语义）。
第九批 5 个模块为：`store/event_row`（EventRecord 与 canonical row 往返）、
`store/events/rows`（事件 Arrow batch 编解码）、`store/files/projected_steps`
（投影 JSON 与 timing 规范化）、`store/catalog/discovery`（候选 source 元数据与格式提示）
以及 `store/catalog/source`（LazySource 能力与解析错误语义）。

## 集成测试文件索引

| 文件 | 覆盖内容 |
|---|---|
| `atif_lance_corpus.rs` | 独立 ATIF corpus（`tests/fixtures/atif/`，8 条 10–20 step 确定性轨迹）的格式转换、Storyline 三表落盘与空间占用 |
| `capture_fixture_corpus.rs` | 复用 `persisting-gateway/tests/fixtures` 的跨组件语料：AgenticMD golden 往返、request/response/provider snapshot/SSE 文本的无损往返（设置最小样本数，防止 fixture 缩减后静默通过） |
| `conversion_semantics.rs` | ATIF、ACTF、OpenAI 的三条直接 Lance 往返与六条有向跨格式转换；对象字段的 `null`/missing 按语义等价比较，数组中的 `null` 保持显著，unknown field 的 JSON Pointer、值与出现次数精确校验 |
| `direct_file_query.rs` | 直接对文件/目录的只读查询（不经过导入） |
| `import_roundtrip_fixtures.rs` | `tests/fixtures/import_roundtrip/` 中 OpenAI corpus / ACTF 经三表 Lance 的无损恢复 |
| `langfuse_backend_faults.rs` | 存储后端故障语义（追加失败、重复、未知错误分类） |
| `production_scale.rs` | 批量追加、epoch fencing、manifest 元数据增长、takeover 隔离 |
| `query_engine.rs` | `ChronicleQueryEngine` 统一 SQL 面（Lance/ATIF/OpenAI/ACTF、`_file_`、只读门禁） |
| `s3_storage.rs` | 真实 S3/MinIO 对象存储契约（默认忽略，见下） |
| `search_integration.rs` | Search 索引与检索（Cargo.toml 显式声明的 `[[test]]`） |
| `unknown_fields_roundtrip.rs` | unknown-fields envelope 在 ATIF、ACTF、OpenAI 跨格式链路中的 JSON Pointer、值与计数保真 |

真实 S3/MinIO 契约测试在隔离的测试前缀下显式运行：

```bash
PCHRONICLE_S3_TEST_URI=s3://bucket/test-prefix \
  cargo test -p persisting-pchronicle --test s3_storage -- --ignored
```

## 约定

1. **测试名描述行为，不描述实现**：`行为_条件_期望`（如
   `newer_epoch_fences_old_publication`、`import_recovers_after_crashed_first_import...`）。
2. **断言精确计数**（行数/记录数/版本号），不用 `is_ok()` 糊弄；corpus 测试设置最小
   样本数量，防止 fixture 被意外缩减后测试仍静默通过。
3. **并发/崩溃语义必须真实竞争或显式故障注入**。不要用共享进程锁把并发测试串行化后
   宣称覆盖了 CAS——`root_write_lock` 是进程级互斥，两个 store 只要共享该锁就不会走到
   对象存储的 ETag/version 条件更新路径。
4. **对象存储语义用 `shared-memory://` scheme 模拟**；真实对象存储行为（条件写、版本、
   覆盖语义）归入 `s3_storage.rs` 的 `--ignored` 契约测试。
5. **回归测试先证明 bug，再修代码**：修复离线语义/字节稳定性/路径穿越类问题时，先加
   能复现的失败测试，再让实现通过它。
6. 基准统一通过 `benchmark/pchronicle/bench.py` 运行；runner 负责设置规模与重复次数、
   调度 Criterion/hyperfine，并生成可比较的 JSON、Markdown 和 HTML 报告。
