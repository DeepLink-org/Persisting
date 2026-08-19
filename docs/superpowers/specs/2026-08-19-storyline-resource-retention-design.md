# Storyline 资源边界与回收设计

## 目标

在不改变现有调用方默认行为的前提下，为 Storyline 导入提供可选的硬资源边界，补齐
`objects.lance` 与旧 physical generation 的 retention 回收，并补强 ATIF presence、对象存储
并发和设计文档的验证。

本设计覆盖以下六项：

1. 单文档、单 chunk 和整次导入的可配置资源限制；
2. ATIF missing/null/value 经 Lance 三表的端到端往返测试；
3. `objects.lance` 使用与三张业务表相同的 vacuum retention；
4. 旧 `generations/<table_generation>/` 目录使用同一 retention 回收；
5. 非空对象存储 replacement 的确定性 CAS 测试与真实 S3/MinIO 多进程契约测试；
6. 修正 Storyline Lance 与 RFC-0001 的已知文档不一致，并记录水位算术依赖。

## 非目标

- 不引入 reader lease、活跃快照注册表或无限期 reader 存活保证。
- 不改变现有 API 的默认资源行为；未配置限制时仍保持无限。
- 不把重复 document id 检测改为概率性指纹集合。
- 不改变 projection fold 算法或完整性验证语义。
- 不进入 TTAS、Queue、Search 或 standalone dlcapt 子系统。

## 导入资源限制

### API

扩展现有 `StorylineContentOptions`，增加五个可选字段：

```rust
pub max_document_rows: Option<usize>,
pub max_document_bytes: Option<usize>,
pub max_chunk_rows: Option<usize>,
pub max_chunk_bytes: Option<usize>,
pub max_import_documents: Option<usize>,
```

所有字段默认均为 `None`。这样现有构造函数、导入方法和默认行为保持不变。任何
`Some(0)` 都在 `StorylineContentOptions::validate` 中被拒绝。

`rows` 定义为一个规范化文档产生的 run、step 和 tool-call 行数之和；`bytes` 定义为
`StorylineDocument` 的 serde JSON 编码字节数。字节计数通过只累计写入长度的 `Write`
实现完成，不构造临时 JSON `Vec<u8>`。

### 语义

- `max_document_rows` 与 `max_document_bytes` 是单文档硬限制。
- `max_chunk_rows` 与 `max_chunk_bytes` 是单 chunk 硬限制。单个文档若本身超过 chunk
  限制，也必须被拒绝，不能作为“超限的第一个文档”放行。
- `max_import_documents` 是整次 stream 的硬限制，并给全局 `document_id` 集合建立明确
  上界。
- duplicate document id 仍以完整字符串精确判断，不使用哈希近似。
- 任一限制失败均返回包含限制名称、实际值和上限的错误，且不移动 `CURRENT`。
- 与现有流式失败语义一致，失败前产生的不可达 Lance versions 或 objects 可由后续
  maintenance 回收；已有已发布快照保持可读。

### Chunk 状态

chunk builder 增加一个 pending document 槽。读取并规范化下一个文档后：

1. 先检查单文档和整次导入限制；
2. 若当前 chunk 非空且加入该文档会超过 chunk 限制，将文档保存到 pending 槽并返回
   当前 chunk；
3. 下一轮优先消费 pending 文档；
4. 若当前 chunk 为空而文档本身超过 chunk 限制，立即报错。

这一状态机保证不丢文档、不超出硬 chunk 上限，也不要求输入 iterator 支持回退。

## Objects vacuum 与 physical generation retention

### Objects vacuum

`maintain` 在对象可达性 prune 和新 `CURRENT` 发布成功后，对 runs、steps、tool_calls、
objects 四个 Lance dataset 使用同一个 `vacuum_older_than`。

`StorylineMaintenanceReport` 新增：

```rust
pub objects: LanceMaintenanceReport,
pub generations_removed: usize,
```

现有 `objects_removed` 保留，用于报告逻辑删除的对象行数；`objects` 报告 Lance old
versions 与物理字节回收结果。

### Generation retention

physical generation 名称已经包含创建时的 Unix 纳秒时间：
`gen-<nanos>-<pid>-<sequence>`。maintenance 在发布新 `CURRENT` 并完成 Lance vacuum 后：

- `vacuum_older_than: None` 时不枚举或删除 generation；
- `Some(retention)` 时计算时间 cutoff；
- 永不删除当前 `table_generation`；
- 只删除名称可严格解析且时间早于 cutoff 的 generation 目录；
- 无法解析的目录跳过，不进行猜测性删除；
- 删除数量写入 `generations_removed`。

这与现有 Lance 时间 retention 采用同一失效契约：超过保留期的旧 reader 不保证继续
读取；保留期内的 reader 所需版本和 physical generation 不应被删除。

## Presence 端到端验证

新增测试构造包含以下状态的 ATIF 输入：

- 根、agent、turn 可选字段的 missing 与显式 null；
- tool result 的 missing、null 与 value；
- trajectory-only identity 和原始 RFC3339 timestamp。

测试路径为：ATIF JSON → Storyline → `StorylineLanceStore` → Storyline → ATIF JSON，最终
对 JSON `Value` 做 `assert_eq!`。这条测试必须经过真实三表编码、Lance 写入、精确 version
读取和三表重建，而不是只调用 Arrow codec。

## 对象存储并发验证

### 确定性 replacement CAS 测试

在现有 `cfg(test)` hook 中增加“读取非空 CURRENT 后、开始数据集 mutation 前”的 barrier。
测试创建两个指向同一 `shared-memory://` root、但持有独立进程锁的 store，使两个 writer
从同一 generation 开始替换不同文档。

预期结果：

- 恰好一个 writer 发布成功；
- 另一个 writer 返回 commit conflict；
- `CURRENT` 指向的四个版本均可打开；
- 基线文档与胜者文档存在，败者未被错误发布；
- 败者从最新 snapshot 重试后，三个文档均存在。

### 真实 S3/MinIO 多进程契约测试

在 `tests/s3_storage.rs` 增加默认 ignored 的父测试和 worker 流程。父测试为每次运行创建
隔离前缀，写入基线文档，然后启动多个当前测试二进制子进程；子进程在父进程释放本地
同步屏障后对同一 S3 prefix 执行 replacement。

真实调度允许两种合法结果：writer 串行后都成功，或重叠 writer 中部分返回明确 conflict。
测试不强制冲突一定发生，但必须验证：没有非 conflict 错误、`CURRENT` 始终可读、基线不丢失、
每个报告成功的文档均可见。确定性 CAS 分支由 shared-memory barrier 测试负责。

## 文档修正

同步修改英文和中文 Storyline Lance 设计文档：

- replacement/delete scope 从 `session_id` 改为 `document_id`；
- 三张索引表均补上 `document_id` BTree；
- 说明 `[previous_fact_rows, fact_rows)` 成立依赖 manifest 强制
  `fact_rows == total_rows()`，且 compaction replacement 行数保持不变；范围读取后的精确
  行数断言是最后一道失败关闭检查。

修正 RFC-0001：ATIF `trajectory_id` 映射到 Storyline wire `trajectory`，`run` 保留给
`run_id`，并同步修正 JSONPath 示例。

## 测试与验收

所有生产行为修改遵循 RED → GREEN：先添加能够因缺失行为失败的测试，再实现最小代码。

定向验收命令：

```text
cargo test -p persisting-pchronicle --lib store::storyline::tests
cargo test -p persisting-pchronicle --test atif_lance_corpus
cargo test -p persisting-pchronicle --test s3_storage
cargo fmt --all -- --check
cargo clippy -p persisting-pchronicle --all-targets -- -D warnings
```

真实 S3/MinIO 多进程测试继续由 `PCHRONICLE_S3_TEST_URI` 显式启用，不纳入默认本地验收。
