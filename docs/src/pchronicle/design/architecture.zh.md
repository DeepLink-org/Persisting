# pChronicle 架构

本文解释 pChronicle 如何存储 Agent 轨迹，并提供有资源限制的读取面。用户工作流属于
[Guides](../guides/index.md)，精确命令属于[Reference](../reference/cli.md)，跨产品 ownership
属于 [System Design](../../system-design/architecture.md)。

![pChronicle 产品边界](../../assets/diagrams/persisting/pchronicle-product.svg)

## 产品边界

pChronicle 是 Agent 轨迹**存储引擎**。它可以作为本地工具使用，也可以平台化部署在多条
path 前面。CLI、Web、Agent 和 Warehouse 都是引擎的客户端，不是并列产品。

引擎 API 是：

```text
open(path) → pin Snapshot → 发现 / 定位 / 分析（写入路径上再 append）
```

**Dataset 就是 path**：规范化的本地路径或对象存储 URI（`s3://`、`az://`、`gs://`）。
mount 名、`@alias`、Directory 的 library 名都是 locator。解析完成后引擎只看见 path。
凭据不得嵌入这条 path。

| 形态 | 用途 | 持久状态 |
| --- | --- | --- |
| 直接 Dataset | 检查本地路径或 S3 prefix | Dataset 外无状态 |
| 原生 Dataset | 接收 canonical event 或 create-only import | Dataset manifest 与 version |
| 本地默认 Dataset | 为一个本地 root 省略 Dataset 参数 | 用户配置中的规范化路径 |
| 只读 Warehouse | 为 Web/API review 静态挂载 path；可选 Directory 做 path ACL | 配置与可重建 cache |

pChronicle 不是 scheduler、Agent runtime、全局 Dataset 控制面、分布式 SQL 服务或时序数据库。
Warehouse 只接受 loopback bind。未使用 `--catalog-config` 时没有用户鉴权；启用后 Directory
与数据面路由要求用户 ak/sk 请求头，这仍不是公网多租户服务。见
[RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md)。

## 四层

| 层 | 职责 | 不是 |
| --- | --- | --- |
| **Path** | Dataset 身份。本地路径或对象存储 URI。 | mount 名、`@alias`、library 名、`catalog://` URI |
| **Directory**（可选） | 平台寻址：把名字解析成 path，并决定谁能打开。换票后客户端打开 path。 | 第三种 Dataset。不是 Snapshot。 |
| **Snapshot** | 一条 path 上写入与读取之间的同步协议：有哪些 Source、各钉在哪个版本。 | 名叫 Catalog 的产品。不是 Directory 列表。 |
| **Query 面** | 发现（`ls` / `sources`）、定位（`find`）、分析（`query`）。全部相对于已 pin 的 Snapshot。 | Web Explorer 的第四套语义 |

代码里仍可能使用 `DatasetCatalogSnapshot`、`--catalog-config` 等名称。用户文档和 RFC 使用
Path、Directory、Snapshot。

Directory 见 [RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md)。Snapshot 构造见
[Snapshot 设计](catalog.md)。

## 数据层次与 Ownership

```text
writers and importers
  → canonical events and terminal facts
  → logical Run / Step / ToolCall projections
  → exchange representations
  → lineage-bearing revisions
```

| 层次 | Ownership 规则 |
| --- | --- |
| canonical event | 写入时事实；append-oriented 的事实源 |
| logical projection | 规范化、可重建查询视图 |
| exchange representation | import/export 契约；不会静默升级为全局事实 |
| revision | 带 parent Snapshot 和 transform lineage 的派生输出 |

Storyline 是 session-oriented projection。它的 Lance 布局为完整文档重建优化，因此按 session
replace 不是 canonical 高频 append 路径。

## Dataset 寻址

规范化 path 是资源身份。Source path 标识其中一个能独立发现和版本化的表示。外部 ID
保持 Source-local：

```text
(path, source_path, entity_kind, original_id)
```

Warehouse mount name 只是 SQL alias。移动到新 path 后就是不同 Dataset。`catalog://` 是
Directory 解析用的 alias 类型，不是 `DatasetLocation` scheme。

## 读取路径

```text
path（经 Directory 换票或 alias 解析之后）
  → resource-limited discovery
  → pin Snapshot
  → Source pruning and lazy open
  → normalized DataFusion relations
  → resource-limited CLI, API, or Web result
```

一次操作钉住每个 Source 的 version reference。本地文件使用 identity 与 fingerprint，Lance
使用发布的 generation 或 manifest，对象存储在可用时使用 version 或条件 ETag。Snapshot
不声称无关 Source 共享全局事务时间。

定位（`find`）与分析（`query`）共用这个已 pin 的 Snapshot。CLI 与 Web 共用 `find` 表达式、
报告的 scope 和 `snapshot_id`。Web UI 可以对返回字段做高亮和可视截取；这层前端匹配不得
改变命中集合。Web Explorer 是定位层的 UI，不是另一套查询语言。

支持时，规范化关系包括 `sources`、`runs`、`steps`、`tool_calls`、`events` 和
`trajectories`。每个实体关系保留 `source_path`。SQL 只读，并拒绝 DDL、DML、网络函数和
文件函数。

发现与裁剪算法见 [Snapshot 设计](catalog.md)。

## 写入与发布路径

Gateway 和 native writer 先发布 canonical event，再生成派生视图。Snapshot 引用的对象必须
先持久化，最后才能发布可见 pointer。发布失败时，旧 Snapshot 保持可读。

```text
validate event
  → persist payload or content-addressed object
  → append canonical fact
  → publish terminal fact or projection generation
  → expose through a later Snapshot
```

Writer 并发由具体 store contract 定义。Snapshot compare-and-swap 本身不意味着 merge-and-retry。
未发布 version 与不可达 object 需要显式维护路径。

Canonical/projection 边界见[运行存储](trajectory-storage.md)，Storyline 实现见
[Storyline Lance](storyline-lance.md)。

## 只读 Warehouse

Server 静态挂载命名 path。Refresh 先完整构造新 Snapshot，再切换 reader。
Dataset table 先按 Source 裁剪，再打开命中的固定 version；cache 和 routing index 与 Snapshot
generation 绑定。

使用 `--catalog-config` 时，父进程只提供 Directory 列表/换票，自己不打开这些 path。
已授权的 Web 查询在只含该用户 path 的 worker 中执行。CLI 换票后打开票里的 `uri`（一条 path）
并注入存储钥。这是 path 上的平台寻址，不是新的 Dataset 种类。

Web 与 API 是同一读取模型的 consumer，不形成新事实源。未知 API route 保持 error，不进入
SPA fallback；只接受 loopback listener。

用户配置见 [Warehouse 指南](../guides/serve.md)，精确 route 与 Gateway 组合见
[`pchronicle` 参考](../reference/cli.md)。

## 保证与明确不保证

| 领域 | 保证 | 不保证 |
| --- | --- | --- |
| identity | path + Source path + original ID 保持可见 | 外部 ID 全局唯一 |
| 读取一致性 | 一个 Snapshot 内固定 Source reference | 跨 Source 全局事务 |
| 发布 | 新发布成功前旧 Snapshot 保持可读 | 所有 writer 自动 merge retry |
| 查询 | 有资源限制的只读执行 | 任意 mutation 或无限制服务查询 |
| projection | 声明范围内的 lineage 与可重建性 | 没有 generation 记录的 freshness |
| 服务 | loopback-only 静态读取面；Directory 模式下数据面用用户钥鉴权 | 公网多租户 Warehouse |

## 相关设计

- [Snapshot 设计](catalog.md)：discovery、Snapshot 构造、惰性 Source resolve 与裁剪。
- [RFC-0013 path Directory](../../rfcs/0013-pchronicle-warehouse-catalog.md)：名字→path、ACL、换票与 query worker。
- [运行存储](trajectory-storage.md)：canonical fact、存储布局与写入 ownership。
- [Storyline Lance](storyline-lance.md)：三表 projection、内容层、发布与维护。
- [记录数据、视图与版本](../concepts/facts-and-projections.md)：这些层次的用户心智模型。
- [pChronicle Reference](../reference/index.md)：精确 CLI 与格式契约。
