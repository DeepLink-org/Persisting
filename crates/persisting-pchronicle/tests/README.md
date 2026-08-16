# pChronicle 测试布局

pChronicle 的测试分三层，放置位置与运行方式如下。

| 层级 | 位置 | 覆盖对象 | 运行方式 |
|---|---|---|---|
| 单元测试 | `src/**/tests.rs` 或小模块内 `#[cfg(test)] mod tests` | 与被测模块同目录：单模块/单函数行为、格式解析、存储协议、并发语义 | `cargo test -p persisting-pchronicle --lib` |
| 集成测试 | `tests/*.rs` | 跨模块/跨 crate 行为：存储往返、查询引擎、格式 corpus、真实 S3 契约 | `cargo test -p persisting-pchronicle` |
| 基准 | `benches/*`、`benchmark/pchronicle/` | 转换与存储性能回归 | `just benchmark-pchronicle` |

大型存储模块使用邻接测试文件：`store/events/tests.rs` 覆盖 fencing/append，
`store/storyline/tests.rs` 覆盖 CURRENT 原子性，`store/files/tests.rs` 覆盖 `_file_` 裁剪；
较小模块（如 `store/agenticmd_fs.rs`）仍可保留内联 `mod tests`。
crate 级门面行为（格式往返、detect、frontmatter 解析）集中在 `src/tests.rs`。

## 集成测试文件索引

| 文件 | 覆盖内容 |
|---|---|
| `atif_lance_corpus.rs` | 独立 ATIF corpus（`tests/fixtures/atif/`，8 条 10–20 step 确定性轨迹）的格式转换、Storyline 三表落盘与空间占用 |
| `capture_fixture_corpus.rs` | 复用 `persisting-gateway/tests/fixtures` 的跨组件语料：AgenticMD golden 往返、request/response/provider snapshot/SSE 文本的无损往返（设置最小样本数，防止 fixture 缩减后静默通过） |
| `direct_file_query.rs` | 直接对文件/目录的只读查询（不经过导入） |
| `import_roundtrip_fixtures.rs` | `tests/fixtures/import_roundtrip/` 中 OpenAI corpus / ACTF 经三表 Lance 的无损恢复 |
| `langfuse_backend_faults.rs` | 存储后端故障语义（追加失败、重复、未知错误分类） |
| `production_scale.rs` | 批量追加、epoch fencing、manifest 元数据增长、takeover 隔离 |
| `query_engine.rs` | `ChronicleQueryEngine` 统一 SQL 面（Lance/ATIF/OpenAI/ACTF、`_file_`、只读门禁） |
| `s3_storage.rs` | 真实 S3/MinIO 对象存储契约（默认忽略，见下） |
| `search_integration.rs` | Search 索引与检索（Cargo.toml 显式声明的 `[[test]]`） |

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
