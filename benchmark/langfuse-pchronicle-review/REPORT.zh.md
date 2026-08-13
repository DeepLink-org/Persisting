# Langfuse ClickHouse → pChronicle 存储后端可行性评审

评审日期：2026-08-11

> [KNOWN, HIGH] 2026-08-12 的 lazy-catalog 优化已显著改变性能结论；性能章节请以
> [RETEST-2026-08-12.zh.md](RETEST-2026-08-12.zh.md) 为准。本报告的 full-v4
> 功能兼容结论保留为历史基线。

## 先看最强支持理由

[COMPUTED, HIGH] 如果目标被收窄为“低吞吐、追加式、按轨迹回放的原始事件库”，pChronicle 在本次 100k event PoC 中是可用的：113,000 行预载及 30 行 10 eps 在线写入均无 ack 后丢失；写入可见延迟 P95 为 4.13 ms；项目交叉点查为 0 行；SIGKILL-after-ack、writer fencing、固定快照、maintenance 后重启均通过；cold catalog 2.10 s，进程 RSS 1.04 GiB。

[INFERRED, HIGH] 这说明 pChronicle 适合承接 Langfuse 的 canonical/cold trajectory、回放和证据归档角色，也说明“完全不适合 Langfuse 数据”这个更强的否定命题不成立。

## 结论

**[INFERRED, HIGH] NO-GO：在钉住的代码状态和已批准的验收标准下，不应以 pChronicle 直接替换 Langfuse 的 ClickHouse 后端。**

[COMPUTED, HIGH] 直接替换触发了四个立即否决项：重复逻辑版本仍作为物理行暴露；`bookmarked/public` 更新不支持；trace/project/retention 删除不支持；Langfuse dashboard、FTS、去重和 Parquet export 所依赖的关键 ClickHouse 语义不支持。

[COMPUTED, HIGH] 即使只看已可表达的查询，最终记录跑中 pChronicle point/list P95 分别是 ClickHouse 的 82.02×/190.13×，超过“不慢于 5×”门槛。第二次完整 pChronicle 跑的 point P95 为 527.94 ms，说明 500 ms 的绝对 point 门槛也不稳定。

[INFERRED, HIGH] 补齐这些缺口至少需要新的 latest-version/tombstone 投影、typed OLAP projections、ClickHouse 查询语义兼容层、多租户网络服务与 catalog pruning；这已是一个新的多租户 OLAP 产品，而不是有界适配器，因此违反本次评审的停止条件。

[INFERRED, HIGH] 推荐保留 ClickHouse 作为 hot OLAP 和 Langfuse API/UI 的查询后端，把 pChronicle 放在旁路，作为追加式 canonical/cold trajectory store。Postgres、Redis 和现有对象存储保持不变。

## 评审范围与基线

[KNOWN, HIGH] 本评审只讨论替换 ClickHouse。Langfuse 的 Postgres 事务数据、Redis 队列/缓存、S3/对象存储不在替换范围内。Langfuse 官方架构也将这些职责分开：[Architecture](https://langfuse.com/handbook/product-engineering/architecture)、[ClickHouse infrastructure](https://langfuse.com/self-hosting/deployment/infrastructure/clickhouse)。

[KNOWN, HIGH] 钉住的基线为：

- Persisting `94531cf903e5abc336de347588fb1858e9d52b6a`，加评审时已存在、未提交的 catalog/query-engine 实验代码；
- Langfuse `d18a59ad663ffc7c04afc61354186c141b3ec0f3`，工作区干净；
- ClickHouse `25.12.11.4`；
- Langfuse v4 语义。v4 对 ClickHouse 的版本和表结构要求可参见官方 [v3 → v4 upgrade guide](https://langfuse.com/self-hosting/upgrade/upgrade-guides/upgrade-v3-to-v4)。

[KNOWN, HIGH] Persisting 的 TTAS、Queue/Sampler、Search 和独立 `persisting-dlcapt` 不在本评审范围内。FTS 只检查现有非 Search 的 DataFusion/Lance 路径，没有借助另一个搜索子系统掩盖后端缺口。

[KNOWN, HIGH] 新增的实现与证据文件是：

- `crates/persisting-pchronicle/examples/langfuse_backend_feasibility.rs`：有界 backend contract、fixture、语义/性能/资源/maintenance 探针；
- `crates/persisting-pchronicle/tests/langfuse_backend_faults.rs`：ack 后强制终止进程再恢复读取；
- `benchmark/langfuse-pchronicle-review/clickhouse_baseline.py`：相同 fixture 的 ClickHouse 25.12 对照；
- `benchmark/langfuse-pchronicle-review/recorded-results-2026-08-11.json`：最终机器可读结果；
- `benchmark/langfuse-pchronicle-review/README.md`：复现命令。

## 数据映射与 workload

[KNOWN, HIGH] PoC 使用以下映射；完整 Langfuse 逻辑行同时保留在 `payload_json` 中：

| Langfuse | pChronicle |
|---|---|
| `project_id` | `agent_id` |
| `trace_id` | Run / `root_session_id` |
| `span_id` | `call_id` |
| `parent_span_id` | `parent_call_id` |
| 无 trace 的 score/dataset/blob 行 | synthetic Run |

[COMPUTED, HIGH] 两个后端读取的是同一份 238,470,862-byte JSONL，SHA-256 为 `1b2ef8ed2f51e6d85a9907bbbebbdafbc4c961d8c4368b2a20b737f94cfc4c90`。它包含两个 project、100,000 events（100 个重复逻辑版本、99,900 个 distinct logical IDs）、10,000 scores、2,000 dataset-run items 和 1,000 blob-log rows；另含 metadata、tags、tool names、65 KiB I/O 和 12 位小数 cost。200 条 trace 加 synthetic Runs 在 pChronicle 中形成 210 个 source datasets。

[KNOWN, HIGH] 在线阶段持续 3 秒、目标 10 eps，并运行一个并发查询；查询基准每类重复 7 次。该规模对应已批准的 team self-hosted/dev 验收，不代表生产容量。

## 验收结果

[COMPUTED, HIGH] 下表采用最终记录跑；原始摘要在 `recorded-results-2026-08-11.json`。

| Gate / workload | pChronicle | ClickHouse | 判定 |
|---|---:|---:|---|
| 113k 预载 ack loss | 0 | 0 | `[COMPUTED, HIGH]` 通过 |
| 10 eps 在线阶段 | 10.26 eps | 9.86 eps | `[COMPUTED, HIGH]` 通过 |
| 在线 ack loss | 0 / 30 | 0 / 30 | `[COMPUTED, HIGH]` 通过 |
| 可见延迟 P95 | 4.13 ms | 6.51 ms | `[COMPUTED, HIGH]` 通过 ≤2 s |
| 重复版本后的物理/逻辑 event 行 | 100,000 / 99,900 | 99,900 / 99,900 | `[COMPUTED, HIGH]` pChronicle 失败 |
| point P95 | 395.87 ms | 4.83 ms | `[COMPUTED, HIGH]` 绝对值通过，82.02× 相对门槛失败 |
| list P95 | 564.82 ms | 2.97 ms | `[COMPUTED, HIGH]` 绝对值通过，190.13× 相对门槛失败 |
| simple group/facet P95 | 806.70 ms（model group） | 3.47 ms（tag facet） | `[COMPUTED, MED]` 都过 1 s；查询并非完全同构，不用其比值作单独否决 |
| 并发查询 P95 | 350.41 ms | 13.96 ms | `[COMPUTED, HIGH]` pChronicle 为 25.10× |
| dashboard | ClickHouse curried quantile/`WITH FILL` 不支持 | 4.53 ms | `[COMPUTED, HIGH]` 失败 |
| FTS | `hasAllTokens` 不支持 | 3.47 ms | `[COMPUTED, HIGH]` 失败 |
| JSON export TTFB | 429.09 ms / 50k 行 | 68.77 ms / 10k 行 | `[COMPUTED, MED]` 都过 2 s；行数不同，不比较倍数 |
| Parquet export TTFB | 不支持 | 28.27 ms | `[COMPUTED, HIGH]` 失败 |
| update flags | 不支持 | 25.32 ms | `[COMPUTED, HIGH]` 失败 |
| trace/project/retention delete | 全部不支持 | 8.98 / 5.96 / 7.64 ms | `[COMPUTED, HIGH]` 失败 |
| catalog cold start | 2.10 s | N/A（常驻服务） | `[COMPUTED, HIGH]` 通过 ≤30 s |
| RSS / resident memory | 1,117,782,016 B | 1,422,721,024 B | `[COMPUTED, MED]` 两者采集边界不同；pChronicle 通过 ≤2 GiB |
| maintenance + restart visibility | 38.36 ms + 4.16 s，可见 | N/A | `[COMPUTED, HIGH]` 通过本次故障探针 |
| cross-project negative point | 0 行 | 0 行 | `[COMPUTED, HIGH]` 数据映射级通过 |

[COMPUTED, HIGH] bulk append 吞吐为 pChronicle 9,374.59 rows/s、ClickHouse 73,449.48 rows/s，ClickHouse 高 7.83×；两者都足以覆盖本次 10 eps 目标，因此吞吐本身不是立即否决项。

[COMPUTED, HIGH] 另一轮相同 fixture 的完整跑中，pChronicle point/list P95 为 527.94/666.71 ms，ClickHouse 为 4.18/4.00 ms。跨跑波动会改变 point 是否略过 500 ms，但不会改变远超 5× 相对门槛的结论。

## 语义兼容矩阵

| Langfuse v4 能力 | 当前 pChronicle | 结论 |
|---|---|---|
| acknowledged append / batch | 原生 append 可用，PoC 零 ack loss | `[COMPUTED, HIGH]` 可适配 |
| latest-version dedup | 明确 at-least-once；重复 `event_id` 是合法物理事实 | `[KNOWN, HIGH]` 不兼容 |
| `events_full` wide typed schema | 只有少量索引列和完整 JSON payload | `[KNOWN, HIGH]` 不兼容 |
| `events_core` truncated projection/MV | 无对应 typed incremental projection | `[KNOWN, HIGH]` 不兼容 |
| scores / dataset-run items / blob log | PoC 只能以 generic event + synthetic Run 保存 | `[COMPUTED, HIGH]` 存得下，不等于 API/查询兼容 |
| `bookmarked` / `public` 更新 | adapter 明确返回 unsupported | `[COMPUTED, HIGH]` 不兼容 |
| trace/project/retention 删除 | 无逻辑 tombstone/delete contract | `[KNOWN, HIGH]` 不兼容 |
| point/list | 可由少量索引列表达 | `[COMPUTED, HIGH]` 功能可做，性能门槛失败 |
| tags/metadata/tools/cost facets | 主要埋在 JSON；无 Array/Map/Decimal 等同构列 | `[KNOWN, HIGH]` 不兼容 |
| FTS | 非 Search 路径不支持 Langfuse token SQL | `[COMPUTED, HIGH]` 不兼容 |
| dashboards/session aggregates | `argMaxIf`、`sumMap`、curried quantile、`WITH FILL` 等不兼容 | `[COMPUTED, HIGH]` 不兼容 |
| JSONEachRow export | 可流式输出 generic event JSONL | `[COMPUTED, HIGH]` 格式/列语义仍不等同 |
| Parquet export | public query adapter 不支持 | `[COMPUTED, HIGH]` 不兼容 |
| Langfuse migrations/direct schema assumptions | 无 ClickHouse-compatible schema/migration surface | `[KNOWN, HIGH]` 不兼容 |

## 静态代码证据

[KNOWN, HIGH] Langfuse `events_full` 是 wide typed OLAP 表：`Array`/`Map`/`Decimal(18,12)`、materialized cost、bloom/minmax/text indexes、`ReplacingMergeTree(event_ts, is_deleted)`，且主键/排序键以 project、minute、trace、span、time 为中心。见相邻 Langfuse 仓库的 `packages/shared/clickhouse/migrations/unclustered/0039_create_events_full.up.sql:1`。

[KNOWN, HIGH] `events_core` 由增量 materialized view 从 full 表生成，并将 I/O 和 metadata 值截断到 200 字符，承担轻量列表查询。见相邻 Langfuse 仓库的 `packages/shared/clickhouse/migrations/unclustered/0041_create_events_core_mv.up.sql:1`。

[KNOWN, HIGH] Langfuse writer 为 events、scores、dataset-run items 和 blob log 提供 batch/retry/async acknowledged inserts；client 强制 `async_insert=1`、`wait_for_async_insert=1`。见相邻 Langfuse 仓库的 `worker/src/services/ClickhouseWriter/index.ts:35` 和 `packages/shared/src/server/clickhouse/client.ts:190`。

[KNOWN, HIGH] Langfuse repository 会同步更新 full/core 的 `bookmarked/public`，并按 project+trace、project、project+time 执行删除；同时提供 JSON/raw text/Parquet 流式 export。见相邻 Langfuse 仓库的 `packages/shared/src/server/repositories/events.ts:1948`、`:2246`、`:2334`、`:2400`、`:3139`。

[KNOWN, HIGH] query builder 使用 `argMaxIf`、`sumMap`、`groupUniqArray*` 和 `LIMIT 1 BY`，FTS 使用 `tokens` + `hasAllTokens` 再叠加精确 predicate。见相邻 Langfuse 仓库的 `packages/shared/src/server/queries/clickhouse-sql/event-query-builder.ts:522`、`:710`、`:1437` 和 `packages/shared/src/server/queries/clickhouse-sql/fts.ts:74`。

[COMPUTED, HIGH] 对 `web/`、`worker/`、`packages/shared/` 下 `.ts/.tsx` 的词法扫描得到 423 个文件含 ClickHouse 大小写变体；`LIMIT 1 BY` 53 次、`argMaxIf` 35 次、`sumMap` 38 次、`hasAllTokens` 16 次、`WITH FILL` 21 次。这些是包含测试/注释的 occurrence counts，不是迁移工期估算，但足以反驳“只替换一个数据库 client 即可”的假设。

[KNOWN, HIGH] pChronicle 的 canonical Lance writer 自述为 at-least-once、append-only，重复 `event_id` 每次都成为物理行；重维护不在 ingestion path。见 `crates/persisting-pchronicle/src/store/raw_event_lance.rs:147`、`:219`。

[KNOWN, HIGH] pChronicle `EventRow` 只有 seq、标识/时间、kind/source、agent/session/call/trace/model 等索引字段，其他内容进入 `payload_json`。见 `crates/persisting-pchronicle/src/store/event_row.rs:7`。

[KNOWN, HIGH] query engine 是 read-only，只允许单条 `SELECT/VALUES/DESCRIBE/EXPLAIN`。见 `crates/persisting-pchronicle/src/store/query_engine.rs:93`、`:569`。

[KNOWN, HIGH] 当前实验 catalog 会在 snapshot discovery 时打开每个 source，并对 event source 执行 `SELECT * FROM events ORDER BY seq`、collect 后物化 normalized MemTables。见 `crates/persisting-pchronicle/src/store/catalog.rs:1`、`:178`、`:681`、`:764`。

[INFERRED, HIGH] Langfuse 的主访问模式是 project/time 范围 OLAP；当前 pChronicle 以 Run 为物理单元并在 catalog cold path 打开、归一化每个 source。210 sources 尚可在 2.10 s 打开，但该机制没有给出 source 数量增长时的稳定上界，因此不能从本次小规模结果外推到长期多租户项目。

## SQL 兼容探针

[COMPUTED, HIGH] 当前 DataFusion 路径无法 plan：`approx_top_k`、`argMax`、`arrayJoin`、`hasAllTokens`、`JSONExtractString`、`LIMIT 1 BY`、curried `quantile`、`sumMap`、`WITH FILL`。

[COMPUTED, HIGH] `FINAL` 和 `PREWHERE` 文本在探针中没有报 parser/planner error。

[INFERRED, HIGH] 这不证明语义兼容：`FINAL` 可能被当作 alias/no-op 接受，而 `PREWHERE` 能被解析也不意味着拥有 ClickHouse 的存储级 prewhere 和 latest-version 行为。因此二者没有记为通过。

## ClickHouse best-practices 检查

[KNOWN, HIGH] 已逐项检查该仓库 skill 中的 28 条规则。对本评审有直接影响的结论如下：

- [KNOWN, HIGH] 按 `schema-pk-plan-before-creation`、`schema-pk-prioritize-filters`、`schema-pk-filter-on-orderby`、`schema-pk-cardinality-order`，对照 schema 保留了 Langfuse 的 project/time/trace 优先顺序；这正是 point/list/facet 快的关键结构之一。
- [KNOWN, HIGH] 按 `schema-types-native-types`、`schema-types-lowcardinality`、`schema-types-minimize-bitwidth`、`schema-json-when-to-use`，常用过滤/聚合字段使用 native Array/Map/Decimal/LowCardinality；JSON 只作为完整 payload，不替代 typed projection。pChronicle 当前恰好缺这层 projection。
- [KNOWN, HIGH] 按 `schema-partition-lifecycle`、`schema-partition-low-cardinality`、`schema-partition-query-tradeoffs`、`schema-partition-start-without`，复核了 Langfuse 的月分区与 retention 模型；本 PoC 没有为 pChronicle 创造第二套分区系统。
- [KNOWN, HIGH] 按 `insert-batch-size`、`insert-async-small-batches`，ClickHouse 对照使用 batch 和 `async_insert=1, wait_for_async_insert=1`。`insert-format-native` 建议更高效的 Native 格式，但对照保留 Langfuse 实际 `JSONEachRow`，避免用更优但非现状的 producer 美化基线。
- [KNOWN, HIGH] 按 `insert-optimize-avoid-final`、`insert-mutation-avoid-update`、`insert-mutation-avoid-delete`，频繁 mutation/`FINAL` 应慎用；但 Langfuse 已公开要求 bookmarks/public/delete/retention 语义，不能以“最佳实践”名义删掉。可行的替代仍需版本行+tombstone+latest projection，而当前 pChronicle 没有。
- [KNOWN, HIGH] 按 `query-index-skipping-indices`、`query-mv-incremental`，对照保留 text/bloom/minmax indexes 与 full→core 增量 MV。`query-mv-refreshable` 不适用于这条 ingestion path。
- [KNOWN, HIGH] `query-join-filter-before`、`query-join-consider-alternatives`、`query-join-use-any`、`query-join-choose-algorithm`、`query-join-null-handling` 已检查；本次 point/list/facet 对照没有 join，故它们不是判定因素。
- [KNOWN, HIGH] `schema-types-avoid-nullable` 和 `schema-types-enum` 已检查；Langfuse schema 的 nullable 时间/版本字段和 LowCardinality strings 属于现有兼容约束，本 PoC 不借机重构它们。

## 故障、隔离与安全

[COMPUTED, HIGH] `langfuse_backend_faults` 通过 2/2：子进程 append 并输出 ack 后被强制终止，新的 process/runtime 仍读到唯一目标行。

[COMPUTED, HIGH] targeted regression tests 通过：`production_scale` 5 passed、1 ignored；`query_engine` 14 passed；`store::catalog` filter 6 passed。覆盖 writer fencing、固定 snapshot 在 append 期间不漂移、独立 Runs、maintenance/restart 和 read-only SQL boundary。

[KNOWN, HIGH] 当前 pChronicle server 是 loopback-only single-user UI，并明确因为没有 authentication 而拒绝 non-loopback bind；它提供 query/export/maintenance routes，但没有供 Langfuse web/worker 使用的 canonical authenticated network append service。见 `crates/persisting-pchronicle-server/src/lib.rs:1`、`:155`、`:168`。

[COMPUTED, HIGH] PoC 中 `project_id → agent_id` 且 cross-project negative point 为 0，证明当前测试 adapter 没有在该查询中串项目。

[INFERRED, HIGH] 这不等于服务级 tenant isolation：共享 catalog 能看见多个项目，本身没有 Langfuse project RBAC/row policy。若暴露任意 SQL endpoint，调用者可能绕过 adapter 的 project predicate。

[INFERRED, HIGH] 因此未来旁路 dual-write 只能使用服务端固定 endpoint、强认证（mTLS 或短期 service token）、每请求显式 project scope、最小化 SQL surface、传输/静态加密和无 payload/credential 日志；不得允许 tenant 自定义出站 host。此次实现没有新增生产网络端点或扩大权限。

## 为什么不是“再补几个函数”

[INFERRED, HIGH] 满足 full Langfuse v4 至少要新增以下五个耦合层：

1. event version、logical tombstone、latest-row projection，以及 update/delete/retention 的一致性契约；
2. `events_full/events_core/scores/dataset_run_items/blob_log` 的 typed projections、索引、迁移和生命周期；
3. dashboard、sessions、facets、FTS、export 对应的 SQL/执行语义和差分测试；
4. web/worker 可用的 authenticated network write/query service 与 tenant authorization；
5. 跨大量 Runs 的 durable metadata、source pruning、增量 catalog，而不是 cold path 全打开和全 normalize。

[INFERRED, HIGH] 这些层彼此依赖：没有 version projection 就无法正确聚合；没有 tenant-aware service 就不能部署；没有 typed/pruned projection 就无法达到现有查询延迟。它们构成产品级扩展，不属于批准的 bounded integration。

## 推荐落点

[INFERRED, HIGH] 建议采用并行角色，而非替换角色：

- ClickHouse 继续承载 Langfuse hot OLAP、lists/facets/dashboards/FTS、更新删除以及 JSON/Parquet export；
- pChronicle 接收原始、追加式 trajectory 副本，服务 replay、审计证据、冷归档、离线研究；
- dual-write 失败必须有独立重试/回补水位，不能让 pChronicle ack 替代现有 ClickHouse ack；
- project deletion/retention 必须在两端有可审计的 lifecycle 协议后，才可处理真实租户数据。

[INFERRED, MED] 一个后续有界 pilot 可以只做“单 project、原始 event dual-write + trace replay”，不接管 dashboards、FTS、facets、updates/deletes，也不宣称它是 Langfuse analytics backend。

## 限制与未知边界

[KNOWN, HIGH] 记录环境为 Apple M4、24 GiB RAM、macOS 26.5；pChronicle 是本地 release binary，ClickHouse 是本地 Docker container。没有隔离所有后台负载，因此数值不是可发布的容量基准。

[KNOWN, HIGH] 本次只有单进程、一个并发查询、200 traces/210 sources、两次主要完整跑；没有 HA、远端对象存储故障、长期 compaction、百万级 Runs、多日 retention 或真实租户 payload。

[KNOWN, HIGH] ClickHouse PoC schema 是 Langfuse v4 相关模式的代表性子集，不是完整部署；full API/UI 没有做 E2E，因为静态兼容矩阵已触发 hard NO-GO。该限制不会反转 mutation/dedup/SQL/export 的确定性失败，但会限制对所有边缘查询的覆盖度。

[KNOWN, HIGH] pChronicle catalog/query-engine 基于评审时未提交的实验代码。结论只对上文钉住的 snapshot 有效，后续实现变化必须重跑同一套 fixture、fault tests 和语义矩阵。

[RULES I BROKE]: none
