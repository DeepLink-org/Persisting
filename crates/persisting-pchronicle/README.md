# pChronicle

**Persisting 的结构化轨迹与 Dataset 数据层。**

拥有轨迹领域模型、磁盘格式、Lance 持久化、数据源发现、DataFusion 查询、格式交换
和可重建投影。其他 crate 可以生产或消费轨迹，但不应再定义第二套存储格式或轨迹
协议 DTO。

不拥有产品 CLI、只读 Warehouse HTTP、Web UI，也不拥有 Run 的启动与编排。
[`persisting-pchronicle-cli`](../persisting-pchronicle-cli/README.md) 拥有
`pchronicle` 命令、loopback API 与嵌入式静态资源。
[`pchronicle-web`](../../pchronicle-web/README.md) 拥有浏览前端。
pVisor / Gateway 生产 canonical events；pPilot 编排多个 Run。

Canonical Event 与 Storyline 分别在事实层和交换/分析层保持权威，关系是单向投影：
`events.lance` 是 append-only 运行时事实源；`StorylineDocument` 是与 ATIF v1.7
对齐的权威轨迹模型和外围格式转换枢纽。AgenticMD 只是 Storyline 的人类可读
Markdown 编码。

默认功能面通过四个模块组织：`model`、`document`、`storage`、`query`。外围 wire
DTO、低层 parser、Arrow codec 和 DataFusion provider 保持私有。`search` 是独立
feature。错误门面保持轻量：公开 `Result<T>` 精确等同于 `anyhow::Result<T>`。

Storyline 大内容恢复使用 Lance 的批量 Blob 读取计划，让引擎合并 packed Blob
的读取并调度并发 I/O；按流消费结果，I/O 调度缓冲设为 16 MiB，仍逐对象验证长度和
BLAKE3 校验和。这个缓冲不是完整查询的内存上限，返回的解压内容仍需驻留内存。

Lance 11 的输入预算可用于分批维护 Storyline：

```rust,ignore
store.maintain(&LanceMaintenanceOptions {
    target_rows_per_fragment: 100_000,
    max_compaction_source_rows: Some(500_000),
    max_compaction_source_bytes: Some(256 * 1024 * 1024),
    vacuum_older_than: None,
    ..Default::default()
}).await?;
```

预算按每张 Storyline 表、每次调用独立计算，默认不限制；它限制 compaction 输入，
不限制索引维护、GC、总内存或独立 Blob v2 文件的 I/O。Lance 不会拆开超预算的任务，
如果没有任务能放进预算，本次 compaction 可以不做任何合并，需要提高预算；字节预算
要求源文件已有大小元数据。Raw Event 跨 segment 的维护使用不同路径，会明确拒绝
这两个预算参数。每次成功维护仍通过 CURRENT 原子发布快照。

## Develop

```bash
just test persisting-pchronicle
# or: just test-crate pchronicle
just proptest pchronicle
```

## Links

- [pChronicle overview](../../docs/src/pchronicle/index.zh.md)
- [产品架构](../../docs/src/pchronicle/design/architecture.zh.md)
- [记录数据、视图与版本](../../docs/src/pchronicle/concepts/facts-and-projections.zh.md)
- [pChronicle CLI](../../docs/src/pchronicle/reference/cli.zh.md)
- [RFC-0003 ownership](../../docs/src/rfcs/0003-pchronicle-ownership.md)
- [`persisting-pchronicle-cli`](../persisting-pchronicle-cli/README.md)
