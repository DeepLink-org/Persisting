# pChronicle Server source-routing 加速复测

日期：2026-08-12

## 结论

本轮 Server 加速把可唯一定位的 warm point query 从 pChronicle 原来的 `258.13ms` 降到
HTTP P95 `4.72ms`，与此前 ClickHouse `4.83ms` 基线处于同一数量级。项目 list 和 aggregate
分别降到 `151.05ms` 和 `165.41ms`，绝对延迟可用，但仍约为 ClickHouse 的 `50.8x` 和
`47.7x`。

因此 source-routing index 已经消除了 point query 的主要 fan-out 成本；项目级扫描仍需要
访问 106 个 source，剩余差距主要来自多 source scan/merge/sort，而不是 Catalog 初始化。

## 实现边界

- `DatasetCatalogSnapshot`、`CatalogDataset`、`DiscoveredSource` 和持久化格式未改变。
- `CatalogRuntime` 同代持有可重建的 `ServerAcceleration`。
- Run、event identity、event partition 分别 lazy single-flight 构建。
- value 使用每代随机 keyed 64-bit fingerprint，collision 只扩大 source 候选；原 SQL 谓词
  负责最终过滤。
- 只改写简单单表查询的顶层必要等值/`IN` 条件；不确定时回退原 SQL。
- 索引构建使用 Arrow stream，并限制为 100 万行、100 万 distinct value。
- refresh 原子发布新 snapshot/engine/空索引，旧索引不会跨 generation 复用。

## 113k 行 release 结果

| 场景 | 结果 |
|---|---:|
| Catalog cold（3 次中位数） | `37.15ms` |
| cold identity point，含首次 identity index | `134.26–166.17ms`，中位数 `159.45ms` |
| Catalog + cold identity point（中位数相加） | `196.60ms` |
| warm point HTTP P50 / P95 | `4.15 / 4.72ms` |
| 首次 project list，含 partition index | `312.94ms` |
| warm project list HTTP P50 / P95 | `147.24 / 151.05ms` |
| warm project aggregate HTTP P50 / P95 | `158.63 / 165.41ms` |

Warm 数字各采样 20 次。point 命中 1 个 source；project-a list/aggregate 命中 106 个 source。

## 与 ClickHouse 基线比较

| 场景 | pChronicle Server P95 | ClickHouse P95 | 比率 |
|---|---:|---:|---:|
| point | `4.72ms` | `4.83ms` | `0.98x` |
| project list | `151.05ms` | `2.97ms` | `50.84x` |
| project aggregate | `165.41ms` | `3.47ms` | `47.72x` |

这里 pChronicle 包含 Axum 和 loopback HTTP 开销，ClickHouse 数字复用 2026-08-11 的相同
fixture 基线，并非同轮并发 A/B。因此 point 的约 2% 差异只能解释为接近持平，不能解释为
严格引擎胜负。

## 内存说明

partition index 记录 113,031 行、211 个 source、213 个 distinct fingerprint；identity index
记录相同行数和 113,132 个 distinct fingerprint。fresh process 中 Catalog 后 RSS 为
`26.2MiB`，完成 identity index 与 point query 后为 `101.9MiB`。

这个 RSS 增量同时包含 211 个 LazySource 的首次解析、Lance/DataFusion scan buffer、索引和
allocator retention，不能当作索引对象自身大小。两级 lazy index 的意义是：只运行项目列表
时不会建立 113k-value identity map；超出硬限制时索引不会发布，查询回退原 fan-out 路径。

## 验证

- `cargo test -p persisting-pchronicle-server --lib`：21 passed。
- `cargo clippy -p persisting-pchronicle-server --all-targets -- -D warnings`：passed。
- 真实双 `events.lance` 测试验证 point/project 路由、结果等价和两级 lazy build。
- Server 集成测试验证 `_file_` 注入、quoted alias、显式 `_file_` 保留以及 refresh 清空索引。

机器可读数据见
[`server-acceleration-results-2026-08-12.json`](server-acceleration-results-2026-08-12.json)。
