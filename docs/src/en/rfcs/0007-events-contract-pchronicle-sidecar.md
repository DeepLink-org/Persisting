# RFC-0007: 事件契约与 pChronicle Sidecar 边界

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Date** | 2026-08-16 |
| **Components** | `persisting-events` · pChronicle · Gateway |
| **Amends** | [RFC-0002 Events](0002-events-format.md) · [RFC-0003 pChronicle ownership](0003-pchronicle-ownership.md) |
| **Related** | [端到端架构](../system-design/architecture.md) · [轨迹存储](../pchronicle/design/trajectory-storage.md) |

## 摘要

Persisting 将运行时事件的逻辑契约从存储实现中拆出，由唯一新增 crate
`persisting-events` 拥有。Gateway、事件 producer 与 pChronicle 共享该契约，但只有
pChronicle 拥有 Lance、DataFusion、对象存储、Catalog、查询与投影实现。

本地集成方可以启动 `pchronicle serve --control 127.0.0.1:0 DATASET`，
通过带版本和认证令牌的 Control 协议提交事件并等待 durable acknowledgement。
存储实现由服务进程拥有，客户端只依赖逻辑事件与协议类型。

## 动机

逻辑事件描述“发生了什么”，物理存储实现决定“如何落盘、查询和投影”。将两者分离，
让 producer 可以发布事件而无需依赖具体存储后端，同时继续使用同一种公共事件类型。
pChronicle 保留结构化轨迹的持久化和读取职责。

## 决策

### 1. `persisting-events` 拥有逻辑事件契约

`persisting-events` 是存储无关的公共契约包，拥有：

- `EventIdentity`：`event_id`、`run_id`、`attempt_id`、Story/Turn identity、producer
  与观测时间；
- `EventRecord`：扁平 identity、`seq`、`source`、`kind`、关联字段与 JSON payload；
- 不涉及存储引擎的信封校验；
- 可选 `control` feature 下的 pChronicle control 消息、trait 与进程 client。

默认 feature 只提供事件数据模型。`control` feature 可以依赖 Agent control 类型和 Tokio，
但 MUST NOT 引入 Lance、Arrow、DataFusion、对象存储 SDK 或 pChronicle 实现。

`EventRecord` 是进程内和进程间共同使用的逻辑记录，不是物理存储 schema。事件所属的
Run、Attempt 或 Session 与 producer 定义的 `seq` 一起形成排序上下文；wall-clock
timestamp 只用于关联和展示，不作为顺序真相。

### 2. pChronicle 拥有物理存储与持久解释

pChronicle 消费 `persisting-events::EventRecord`，并独占维护：

- `EventRow`、Arrow schema 与逻辑记录到物理行的映射；
- Lance/Vortex 等物理后端、writer fencing、manifest、compaction 与 vacuum；
- Catalog、query、replay、格式转换、revision 和派生视图；
- `pchronicle serve` 内嵌的 Control 服务以及 append 成功的 durable acknowledgement。

RFC-0003 中“pChronicle 拥有轨迹格式”的约束仍适用于物理 schema、交换格式与转换。
其中“pChronicle 唯一定义 `EventRecord`”以及“所有调用方直接依赖 pChronicle 类型”的部分
由本 RFC 修订。

### 3. 本地 producer 通过 Control 服务提交

`pchronicle serve --control 127.0.0.1:0 DATASET` 提供本地持久化入口。
`persisting-events` 的进程 client 负责启动子进程、握手与请求关联。
子进程退出、握手失败或协议版本不兼容都使持久化请求显式失败。

使用此协议的 producer 提交 `EventRecord`；Lance row、DataFusion 查询与对象存储
实现由 pChronicle 管理。

### 4. Control 协议随事件契约发布

不创建 `persisting-pchronicle-client`。control client、协议信封、Attempt/lease/commit
消息和内存测试实现位于 `persisting-events` 的可选 `control` feature 中。这些类型描述
producer 到持久化服务的边界，本身不实现存储。

当前协议版本为 `2`，使用 loopback TCP 上的逐行 JSON frame。sidecar 在 stdout 发布
一次 readiness 记录，包含 endpoint、协议版本和每进程随机认证令牌。每个请求携带版本、
request ID 与令牌；client 校验响应版本和 request ID。frame 大小有明确上限。

这是本地子进程协议，不是面向不可信网络的远程公共 API。跨主机部署需要另行定义传输
安全、身份和可用性契约。

### 5. ACK、背压与不确定性

Gateway 到存储的 append 队列是有界的。提交被拒绝与提交后的结果不确定必须区分。
只有 pChronicle 完成 append 并返回成功时，事件才视为 durable。

连接中断、写入错误或 ACK 丢失无法证明事件未提交，必须分类为 unknown。此时 producer
消耗该序号，不能把不确定写入伪装为“肯定未写”。事实层仍允许 at-least-once 与重复
`event_id`；exactly-once 和热路径去重不属于该协议。

同一 control sidecar 也承载 Run lease、Attempt active/heartbeat/terminal 和 Run commit。
这些控制记录的 fencing/CAS 语义保持不变。

## 依赖边界

```text
Gateway / local event producers
          │
          │ EventRecord + optional control protocol
          ▼
  persisting-events
          │
          │ versioned authenticated local IPC
          ▼
  pchronicle serve process (Control enabled)
          │
          ├── EventRow / Arrow
          ├── Lance or another storage backend
          └── Catalog / query / projection
```

`persisting-events` 的默认 feature 和可选 `control` feature 均不得引入 Lance、Arrow、
DataFusion 或云对象存储 SDK。物理存储与查询实现只由 pChronicle 维护。

## 兼容与迁移

- `EventRecord` 的 JSON 顶层字段保持扁平，移动 crate 不改变既有 wire 形状；
- pChronicle 对外 re-export 公共事件类型，允许调用方渐进迁移 import；
- `persisting-pchronicle-client` 被删除，使用者改为
  `persisting-events = { features = ["control"] }`；
- pChronicle 的既有 Lance dataset、目录布局和 replay 语义不因这次 crate 拆分而变化。

## 被否决的方案

| 方案 | 原因 |
|---|---|
| producer 内嵌另一套 Lance adapter | producer 与存储引擎生命周期、feature 和依赖重新耦合 |
| `EventRecord` 继续由 pChronicle 定义 | producer 为使用基础事件信封被迫依赖存储产品 |
| 单独保留 `persisting-pchronicle-client` | 协议包过碎；control 契约可以作为事件边界的可选 feature |
| 各集成方分别实现 IPC client | 会产生协议漂移、重复认证与错误语义 |
| 仅依赖离线导入而无实时 Control 协议 | 缺少运行期 durable ACK、fencing 与统一 canonical append 语义 |

## 验收条件

- Workspace 只新增 `persisting-events`，不存在 `persisting-pchronicle-client`；
- Gateway、producer、pChronicle 复用同一 `EventRecord`，没有同构公共事件 struct；
- `persisting-events` 不直接依赖 pChronicle 存储 API，也不链接 Lance/DataFusion；
- 进程 client 能够启动 Control 服务，持久写入有序事件并发布 Attempt 终态；
- 协议版本、认证、request correlation、frame limit、拒绝与 unknown 写入语义有测试；
- pChronicle 的原有 replay、query 与物理存储测试继续通过。

## Changelog

| Version | Date | Notes |
|---|---|---|
| Accepted | 2026-08-16 | 拆出 `persisting-events`，移除 client crate，确立逻辑事件与 pChronicle 存储服务的边界 |
